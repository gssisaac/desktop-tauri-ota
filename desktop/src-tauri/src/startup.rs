//! Release boot sequence. The main window first shows the bundled launcher page
//! (`desktop/launcher`), which renders the status published here, then navigates to the
//! full-stack server once the already installed bundle is healthy. A newer bundle downloads
//! in the background and stays pending until the user refreshes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::bundle_manager::{self, Progress};
use crate::{api_bridge, windows};

const STATUS_EVENT: &str = "launcher://status";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Checking,
    Downloading,
    Installing,
    Starting,
    Ready,
    Error,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherStatus {
    /// Monotonic, so the page can drop stale snapshots that race with events.
    seq: u64,
    phase: Phase,
    message: String,
    /// 0.0–1.0 while downloading with a known size.
    progress: Option<f64>,
    detail: Option<String>,
}

static STATUS: Mutex<LauncherStatus> = Mutex::new(LauncherStatus {
    seq: 0,
    phase: Phase::Checking,
    message: String::new(),
    progress: None,
    detail: None,
});
static BOOTING: AtomicBool = AtomicBool::new(false);

fn publish(
    handle: &AppHandle,
    phase: Phase,
    message: String,
    progress: Option<f64>,
    detail: Option<String>,
) {
    let status = {
        let mut guard = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        *guard = LauncherStatus {
            seq: guard.seq + 1,
            phase,
            message,
            progress,
            detail,
        };
        guard.clone()
    };
    let _ = handle.emit(STATUS_EVENT, status);
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn publish_progress(handle: &AppHandle, progress: Progress) {
    match progress {
        Progress::Checking => publish(
            handle,
            Phase::Checking,
            "Checking for updates…".into(),
            None,
            None,
        ),
        Progress::Downloading { received, total } => {
            let message = match total {
                Some(total) => format!(
                    "Downloading __APP_NAME__ engine… {:.0} / {:.0} MB",
                    megabytes(received),
                    megabytes(total)
                ),
                None => format!("Downloading __APP_NAME__ engine… {:.0} MB", megabytes(received)),
            };
            let fraction = total
                .filter(|t| *t > 0)
                .map(|t| (received as f64 / t as f64).min(1.0));
            publish(handle, Phase::Downloading, message, fraction, None);
        }
        Progress::Preparing { version } => publish(
            handle,
            Phase::Installing,
            format!("Preparing __APP_NAME__ {version}…"),
            None,
            None,
        ),
        Progress::Installing => publish(
            handle,
            Phase::Installing,
            "Verifying and installing…".into(),
            None,
            None,
        ),
    }
}

const UPDATE_READY_EVENT: &str = "__APP_SLUG__://update-ready";

#[derive(Clone, Serialize)]
struct UpdateReady {
    version: String,
}

fn emit_update_ready(handle: &AppHandle, version: &str) {
    let _ = handle.emit(
        UPDATE_READY_EVENT,
        UpdateReady {
            version: version.to_string(),
        },
    );
}

fn resource_dir(handle: &AppHandle) -> Option<std::path::PathBuf> {
    match handle.path().resource_dir() {
        Ok(dir) => {
            log::info!("resource dir: {}", dir.display());
            Some(dir)
        }
        Err(err) => {
            log::warn!("resource dir unavailable: {err}");
            None
        }
    }
}

fn boot(handle: &AppHandle) -> Result<(), String> {
    let app_version = handle.package_info().version.to_string();
    let resources = resource_dir(handle);
    let on_progress = |progress| publish_progress(handle, progress);

    publish(
        handle,
        Phase::Starting,
        "Starting __APP_NAME__…".into(),
        None,
        None,
    );
    let bundle = match bundle_manager::startup_bundle(resources.as_deref(), &on_progress) {
        Ok(bundle) => bundle,
        Err(err) => {
            log::warn!("no local bundle ({err}); downloading before launch");
            bundle_manager::ensure_bundle(&app_version, &on_progress)?
        }
    };

    publish(
        handle,
        Phase::Starting,
        format!("Starting __APP_NAME__ {}…", bundle.version),
        None,
        None,
    );
    api_bridge::start(&bundle)?;
    bundle_manager::prune_inactive(&bundle);

    let url = format!("{}/", api_bridge::origin())
        .parse()
        .map_err(|e| format!("invalid server URL: {e}"))?;
    publish(handle, Phase::Ready, "Opening __APP_NAME__…".into(), None, None);
    windows::navigate_main_window(handle, url)
}

/// Install/update the engine and start it off the main thread. No-op while a boot runs.
pub fn spawn_boot(handle: AppHandle) {
    if BOOTING.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        let started = boot(&handle);
        if let Err(err) = &started {
            log::error!("startup failed: {err}");
            publish(
                &handle,
                Phase::Error,
                "__APP_NAME__ couldn't start".into(),
                None,
                Some(err.clone()),
            );
        }
        BOOTING.store(false, Ordering::SeqCst);
        if started.is_ok() {
            let app_version = handle.package_info().version.to_string();
            let resources = resource_dir(&handle);
            if let Err(err) =
                bundle_manager::prepare_update(&app_version, resources.as_deref(), &|version| {
                    emit_update_ready(&handle, version);
                })
            {
                log::warn!("background update failed: {err}");
            }
        }
    });
}

#[tauri::command]
pub fn launcher_status() -> LauncherStatus {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[tauri::command]
pub fn launcher_retry(app: AppHandle) {
    spawn_boot(app);
}

/// Restart onto the bundle downloaded in the background, then reload the window.
#[tauri::command]
pub fn apply_pending_update(app: AppHandle) -> Result<(), String> {
    if BOOTING.swap(true, Ordering::SeqCst) {
        return Err("__APP_NAME__ is busy. Try again in a moment.".into());
    }
    let result = (|| {
        let previous = bundle_manager::installed_bundle();
        let bundle = bundle_manager::activate_pending()?;
        match api_bridge::start(&bundle) {
            Ok(()) => {
                bundle_manager::prune_inactive(&bundle);
                windows::reload_main_window(&app)
            }
            Err(err) => {
                if let Some(prev) = previous.as_ref() {
                    let _ = bundle_manager::set_pending(&bundle);
                    if bundle_manager::set_active(prev).is_ok() && api_bridge::start(prev).is_ok() {
                        return Err(format!(
                            "{err}\n\n__APP_NAME__ {} is still running.",
                            prev.version
                        ));
                    }
                }
                Err(err)
            }
        }
    })();
    BOOTING.store(false, Ordering::SeqCst);
    result
}
