//! Release boot sequence. The main window first shows the bundled launcher page
//! (`desktop/launcher`), which renders the status published here, then navigates to the
//! full-stack server once it is healthy.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

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
        Progress::Installing => publish(
            handle,
            Phase::Installing,
            "Verifying and installing…".into(),
            None,
            None,
        ),
    }
}

fn boot(handle: &AppHandle) -> Result<(), String> {
    let app_version = handle.package_info().version.to_string();
    let bundle =
        bundle_manager::ensure_bundle(&app_version, &|progress| publish_progress(handle, progress))?;

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
        if let Err(err) = boot(&handle) {
            log::error!("startup failed: {err}");
            publish(
                &handle,
                Phase::Error,
                "__APP_NAME__ couldn't start".into(),
                None,
                Some(err),
            );
        }
        BOOTING.store(false, Ordering::SeqCst);
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
