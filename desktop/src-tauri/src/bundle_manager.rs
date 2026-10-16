//! Full-stack OTA bundles: a Next.js standalone server (UI + API) zipped, signed and
//! published to R2 by `pnpm release:ota`. The launcher installs them under
//! `~/Library/Application Support/__APP_IDENTIFIER__/bundles/v<version>/`.
//!
//! DMG / NSIS builds also embed the latest bundle (`resources/seed-bundle/`). Startup runs
//! the already installed copy (or that seed, on first launch) immediately, then downloads a
//! newer release in the background. The new copy stays pending until the user refreshes.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::crypto;

/// Bundles carry native modules (better-sqlite3), so each platform has its own release feed.
#[cfg(not(windows))]
pub const MANIFEST_URL: &str = "https://bundles.__APP_DOMAIN__/manifest.json";
#[cfg(windows)]
pub const MANIFEST_URL: &str = "https://bundles.__APP_DOMAIN__/manifest-windows-x64.json";
const MANIFEST_TIMEOUT: Duration = Duration::from_millis(2500);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Entry point every bundle must contain (Next.js standalone server).
const SERVER_ENTRY: &str = "server.js";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtaManifest {
    pub version: String,
    #[serde(default)]
    pub min_app_version: Option<String>,
    pub url: String,
    pub sha256: String,
    pub signature: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActivePointer {
    version: String,
    path: String,
}

/// An installed bundle the launcher can run.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub version: String,
    pub dir: PathBuf,
}

impl Bundle {
    pub fn server_entry(&self) -> PathBuf {
        self.dir.join(SERVER_ENTRY)
    }
}

pub enum Progress {
    Checking,
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    /// Unpacking the bundle shipped inside the installer. No network.
    Preparing {
        version: String,
    },
    Installing,
}

/// Serializes install, activate, and prune. Not held across the network download.
static BUNDLE_OP: Mutex<()> = Mutex::new(());

fn lock_bundles() -> std::sync::MutexGuard<'static, ()> {
    BUNDLE_OP.lock().unwrap_or_else(|err| err.into_inner())
}

pub fn app_data_dir() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|d| d.join("__APP_IDENTIFIER__"))
        .ok_or_else(|| "Could not resolve app data directory".to_string())
}

fn bundles_root() -> Result<PathBuf, String> {
    let root = app_data_dir()?.join("bundles");
    fs::create_dir_all(&root).map_err(|e| format!("create bundles dir: {e}"))?;
    Ok(root)
}

fn active_pointer_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("active.json"))
}

fn pending_pointer_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("pending.json"))
}

fn write_active(version: &str, bundle_dir: &Path) -> Result<(), String> {
    let data_dir = app_data_dir()?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("create data dir: {e}"))?;
    let ptr = ActivePointer {
        version: version.to_string(),
        path: bundle_dir.display().to_string(),
    };
    let json = serde_json::to_string_pretty(&ptr).map_err(|e| e.to_string())? + "\n";
    fs::write(active_pointer_path()?, json).map_err(|e| e.to_string())
}

fn write_pending(version: &str, bundle_dir: &Path) -> Result<(), String> {
    let data_dir = app_data_dir()?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("create data dir: {e}"))?;
    let ptr = ActivePointer {
        version: version.to_string(),
        path: bundle_dir.display().to_string(),
    };
    let json = serde_json::to_string_pretty(&ptr).map_err(|e| e.to_string())? + "\n";
    fs::write(pending_pointer_path()?, json).map_err(|e| e.to_string())
}

fn clear_pending() {
    if let Ok(path) = pending_pointer_path() {
        let _ = fs::remove_file(path);
    }
}

/// The manifest is unsigned, so its version must not be able to escape `bundles/`.
fn validate_version(version: &str) -> Result<(), String> {
    let ok = !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    if ok {
        Ok(())
    } else {
        Err(format!("invalid bundle version {version:?}"))
    }
}

/// Dotted numeric comparison; pre-release/build suffixes are ignored.
fn version_lt(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x < y;
        }
    }
    false
}

fn version_dir(version: &str) -> Result<PathBuf, String> {
    validate_version(version)?;
    Ok(bundles_root()?.join(format!("v{version}")))
}

fn staging_dir() -> Result<PathBuf, String> {
    Ok(bundles_root()?.join("staging"))
}

fn fetch_manifest() -> Result<OtaManifest, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(MANIFEST_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(MANIFEST_URL)
        .send()
        .map_err(|e| format!("manifest fetch failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("manifest HTTP {}", resp.status()));
    }
    resp.json::<OtaManifest>()
        .map_err(|e| format!("manifest parse failed: {e}"))
}

fn download_bytes(
    url: &str,
    expected_size: Option<u64>,
    on_progress: &dyn Fn(Progress),
) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("bundle download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("bundle HTTP {}", resp.status()));
    }

    let total = resp.content_length().or(expected_size);
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(MAX_BUNDLE_BYTES) as usize);
    let mut chunk = vec![0u8; 256 * 1024];
    let mut last_report = Instant::now();
    on_progress(Progress::Downloading { received: 0, total });
    loop {
        let n = resp
            .read(&mut chunk)
            .map_err(|e| format!("bundle download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() as u64 > MAX_BUNDLE_BYTES {
            return Err("bundle exceeds maximum size".into());
        }
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            on_progress(Progress::Downloading {
                received: bytes.len() as u64,
                total,
            });
            last_report = Instant::now();
        }
    }
    on_progress(Progress::Downloading {
        received: bytes.len() as u64,
        total,
    });
    Ok(bytes)
}

fn extract_zip_secure(zip_bytes: &[u8], dest: &Path) -> Result<(), String> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| format!("clear dest: {e}"))?;
    }
    fs::create_dir_all(dest).map_err(|e| format!("create dest: {e}"))?;

    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("zip open: {e}"))?;
    let dest_canon = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let name = file.name().map_err(|e| format!("zip entry name: {e}"))?;
        let rel = Path::new(name.as_ref());
        if rel.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!("zip slip rejected: {name}"));
        }
        let out_path = dest_canon.join(rel);
        if !out_path.starts_with(&dest_canon) {
            return Err(format!("zip slip rejected: {name}"));
        }
        if file.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| format!("mkdir: {e}"))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
            }
            let mut out = fs::File::create(&out_path).map_err(|e| format!("create file: {e}"))?;
            std::io::copy(&mut file, &mut out).map_err(|e| format!("extract file: {e}"))?;
            // `zip -X` strips Unix modes. node-pty's spawn-helper must be executable.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut mode = file.unix_mode().unwrap_or(0o644);
                if out_path.file_name().and_then(|n| n.to_str()) == Some("spawn-helper") {
                    mode |= 0o755;
                }
                fs::set_permissions(&out_path, fs::Permissions::from_mode(mode))
                    .map_err(|e| format!("chmod {}: {e}", out_path.display()))?;
            }
        }
    }
    Ok(())
}

/// Unpack a verified zip into `bundles/v{version}`. `make_active` switches the running
/// pointer; otherwise the install stays in `pending.json` until the user refreshes.
fn install_zip(
    version: &str,
    zip_bytes: &[u8],
    make_active: bool,
    on_progress: &dyn Fn(Progress),
) -> Result<Bundle, String> {
    validate_version(version)?;
    let version_path = version_dir(version)?;
    if let Some(active) = active_bundle() {
        if active.version == version && active.dir.join(SERVER_ENTRY).is_file() {
            return Ok(active);
        }
        if same_dir(&active.dir, &version_path) {
            return Err("refusing to replace the running bundle".into());
        }
    }

    on_progress(Progress::Installing);
    let stage = staging_dir()?;
    extract_zip_secure(zip_bytes, &stage)?;
    if !stage.join(SERVER_ENTRY).is_file() {
        fs::remove_dir_all(&stage).ok();
        return Err(format!("bundle v{version} has no {SERVER_ENTRY}"));
    }

    if version_path.exists() {
        fs::remove_dir_all(&version_path).map_err(|e| format!("clear old install: {e}"))?;
    }
    fs::rename(&stage, &version_path).map_err(|e| e.to_string())?;

    if make_active {
        write_active(version, &version_path)?;
        if pending_bundle().is_some_and(|pending| pending.version == version) {
            clear_pending();
        }
        log::info!("bundle installed and active: v{version}");
    } else {
        write_pending(version, &version_path)?;
        log::info!("bundle downloaded, waiting for refresh: v{version}");
    }
    Ok(Bundle {
        version: version.to_string(),
        dir: version_path,
    })
}

fn install_manifest(
    manifest: &OtaManifest,
    make_active: bool,
    on_progress: &dyn Fn(Progress),
) -> Result<Bundle, String> {
    let zip_bytes = download_bytes(&manifest.url, manifest.size, on_progress)?;
    crypto::verify_bundle(&zip_bytes, &manifest.sha256, &manifest.signature)?;
    install_zip(&manifest.version, &zip_bytes, make_active, on_progress)
}

fn pointer_bundle(path: Result<PathBuf, String>, label: &str) -> Option<Bundle> {
    let raw = fs::read_to_string(path.ok()?).ok()?;
    let ptr: ActivePointer = serde_json::from_str(&raw).ok()?;
    let dir = PathBuf::from(ptr.path);
    if dir.join(SERVER_ENTRY).is_file() {
        Some(Bundle {
            version: ptr.version,
            dir,
        })
    } else {
        log::warn!(
            "{label} bundle has no {SERVER_ENTRY} ({}); ignoring pointer",
            dir.display()
        );
        None
    }
}

fn active_bundle() -> Option<Bundle> {
    pointer_bundle(active_pointer_path(), "cached")
}

fn pending_bundle() -> Option<Bundle> {
    pointer_bundle(pending_pointer_path(), "pending")
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.file_name().is_some() && a.file_name() == b.file_name()
}

/// The bundle the launcher is running (or would run) right now.
pub fn installed_bundle() -> Option<Bundle> {
    active_bundle()
}

pub fn set_active(bundle: &Bundle) -> Result<(), String> {
    let _guard = lock_bundles();
    write_active(&bundle.version, &bundle.dir)
}

pub fn set_pending(bundle: &Bundle) -> Result<(), String> {
    let _guard = lock_bundles();
    write_pending(&bundle.version, &bundle.dir)
}

fn pending_newer_than(version: &str) -> Option<String> {
    pending_bundle()
        .filter(|pending| version_lt(version, &pending.version))
        .map(|pending| pending.version)
}

/// True when `candidate` is newer than both the running bundle and any pending download.
fn should_install(running_version: &str, candidate: &str) -> bool {
    let floor = pending_newer_than(running_version).unwrap_or_else(|| running_version.to_string());
    version_lt(&floor, candidate)
}

fn find_seed_dir(resource_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        resource_dir.join("resources").join("seed-bundle"),
        resource_dir.join("seed-bundle"),
    ];
    candidates
        .into_iter()
        .find(|dir| dir.join("version").is_file() && dir.join("bundle.zip").is_file())
}

fn seed_version(resource_dir: &Path) -> Option<String> {
    let dir = find_seed_dir(resource_dir)?;
    let raw = fs::read_to_string(dir.join("version")).ok()?;
    let version = raw.trim();
    validate_version(version).ok()?;
    Some(version.to_string())
}

fn install_seed(
    resource_dir: &Path,
    make_active: bool,
    on_progress: &dyn Fn(Progress),
) -> Result<Option<Bundle>, String> {
    let Some(dir) = find_seed_dir(resource_dir) else {
        return Ok(None);
    };
    let version = seed_version(resource_dir)
        .ok_or_else(|| "shipped bundle has an invalid version".to_string())?;
    on_progress(Progress::Preparing {
        version: version.clone(),
    });
    let zip_bytes =
        fs::read(dir.join("bundle.zip")).map_err(|e| format!("read shipped bundle: {e}"))?;
    install_zip(&version, &zip_bytes, make_active, on_progress).map(Some)
}

/// Bundle to launch before any network check: the installed copy, or the one shipped
/// inside the app on first launch.
pub fn startup_bundle(
    resource_dir: Option<&Path>,
    on_progress: &dyn Fn(Progress),
) -> Result<Bundle, String> {
    let _guard = lock_bundles();
    if let Some(active) = active_bundle() {
        log::info!("launching installed bundle v{}", active.version);
        return Ok(active);
    }
    if let Some(resource_dir) = resource_dir {
        match install_seed(resource_dir, true, on_progress) {
            Ok(Some(bundle)) => {
                log::info!("launching shipped bundle v{}", bundle.version);
                return Ok(bundle);
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("shipped bundle could not be installed: {err}");
                return Err(err);
            }
        }
    }
    Err("no __APP_NAME__ engine installed".into())
}

/// Download a newer bundle without switching to it. Calls `on_ready` when a pending
/// version is waiting (including one downloaded on a previous launch).
pub fn prepare_update(
    app_version: &str,
    resource_dir: Option<&Path>,
    on_ready: &dyn Fn(&str),
) -> Result<(), String> {
    let Some(running) = active_bundle() else {
        return Err("no running bundle".into());
    };

    let mut announced = String::new();
    let announce = |announced: &mut String, version: &str| {
        if announced == version {
            return;
        }
        *announced = version.to_string();
        log::info!("update ready: v{version}");
        on_ready(version);
    };

    if let Some(version) = pending_newer_than(&running.version) {
        announce(&mut announced, &version);
    }

    if let Some(resource_dir) = resource_dir {
        if let Some(version) = seed_version(resource_dir) {
            if should_install(&running.version, &version) {
                let guard = lock_bundles();
                let live = active_bundle()
                    .map(|bundle| bundle.version)
                    .unwrap_or_else(|| running.version.clone());
                if should_install(&live, &version) {
                    install_seed(resource_dir, false, &|_| {})?;
                }
                drop(guard);
            }
        }
    }
    let running_version = active_bundle()
        .map(|bundle| bundle.version)
        .unwrap_or_else(|| running.version.clone());
    if let Some(version) = pending_newer_than(&running_version) {
        announce(&mut announced, &version);
    }

    let manifest = match fetch_manifest() {
        Ok(manifest) => manifest,
        Err(err) => {
            log::warn!("update check failed: {err}");
            return Ok(());
        }
    };
    if let Some(min) = manifest.min_app_version.as_deref() {
        if version_lt(app_version, min) {
            log::warn!(
                "bundle v{} needs __APP_NAME__ {min} or newer (this is {app_version})",
                manifest.version
            );
            return Ok(());
        }
    }
    let running_version = active_bundle()
        .map(|bundle| bundle.version)
        .unwrap_or_else(|| running.version.clone());
    if should_install(&running_version, &manifest.version) {
        log::info!("downloading __APP_NAME__ {}", manifest.version);
        let zip_bytes = download_bytes(&manifest.url, manifest.size, &|_| {})?;
        crypto::verify_bundle(&zip_bytes, &manifest.sha256, &manifest.signature)?;
        let _guard = lock_bundles();
        let live = active_bundle()
            .map(|bundle| bundle.version)
            .unwrap_or(running_version);
        if should_install(&live, &manifest.version) {
            install_zip(&manifest.version, &zip_bytes, false, &|_| {})?;
        }
    } else {
        log::info!("no newer bundle than v{running_version}");
    }
    if let Some(active) = active_bundle() {
        if let Some(version) = pending_newer_than(&active.version) {
            announce(&mut announced, &version);
        }
    }
    Ok(())
}

/// Make the pending download the bundle that runs. Caller restarts the server.
pub fn activate_pending() -> Result<Bundle, String> {
    let _guard = lock_bundles();
    let pending = pending_bundle().ok_or_else(|| "No update is ready to refresh.".to_string())?;
    if active_bundle().is_some_and(|active| !version_lt(&active.version, &pending.version)) {
        return Err("No update is ready to refresh.".into());
    }
    write_active(&pending.version, &pending.dir)?;
    clear_pending();
    log::info!("activating downloaded bundle v{}", pending.version);
    Ok(pending)
}

/// Last-resort first launch when nothing is installed and the app shipped no seed.
/// Downloads and activates before returning.
pub fn ensure_bundle(app_version: &str, on_progress: &dyn Fn(Progress)) -> Result<Bundle, String> {
    let _guard = lock_bundles();
    on_progress(Progress::Checking);
    let active = active_bundle();

    let update = fetch_manifest().and_then(|manifest| {
        if active.as_ref().is_some_and(|b| b.version == manifest.version) {
            return Ok(None);
        }
        if let Some(min) = manifest.min_app_version.as_deref() {
            if version_lt(app_version, min) {
                return Err(format!(
                    "engine v{} requires __APP_NAME__ {min} or newer (this is {app_version}); download the latest app from __APP_DOMAIN__",
                    manifest.version
                ));
            }
        }
        install_manifest(&manifest, true, on_progress).map(Some)
    });

    match (update, active) {
        (Ok(Some(bundle)), _) => Ok(bundle),
        (Ok(None), Some(active)) => Ok(active),
        (Err(err), Some(active)) => {
            log::warn!(
                "OTA update skipped, running cached v{}: {err}",
                active.version
            );
            Ok(active)
        }
        (Err(err), None) => Err(format!("Couldn't download the __APP_NAME__ engine: {err}")),
        (Ok(None), None) => Err("no __APP_NAME__ engine installed".into()),
    }
}

fn prune_except_locked(keep: &Bundle) {
    let Ok(root) = bundles_root() else { return };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    let pending = pending_bundle();
    for entry in entries.flatten() {
        let path = entry.path();
        if same_dir(&path, &keep.dir) {
            continue;
        }
        if pending
            .as_ref()
            .is_some_and(|pending| same_dir(&path, &pending.dir))
        {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_bundle_dir = name.starts_with('v') || name == "staging";
        if is_bundle_dir && path.is_dir() {
            match fs::remove_dir_all(&path) {
                Ok(()) => log::info!("pruned old bundle {}", path.display()),
                Err(e) => log::warn!("prune {}: {e}", path.display()),
            }
        }
    }
}

/// Remove every installed bundle except `keep` and a pending update.
pub fn prune_inactive(keep: &Bundle) {
    let _guard = lock_bundles();
    prune_except_locked(keep);
}

#[tauri::command]
pub fn pending_update() -> Option<String> {
    let running = active_bundle()?;
    pending_newer_than(&running.version)
}

#[tauri::command]
pub fn ota_status() -> Result<serde_json::Value, String> {
    let active = active_bundle();
    let pending = active
        .as_ref()
        .and_then(|active| pending_newer_than(&active.version));
    Ok(serde_json::json!({
        "manifestUrl": MANIFEST_URL,
        "activeVersion": active.as_ref().map(|b| b.version.clone()),
        "activePath": active.as_ref().map(|b| b.dir.display().to_string()),
        "pendingVersion": pending,
    }))
}

#[cfg(test)]
mod tests {
    use super::version_lt;

    #[test]
    fn orders_dotted_versions() {
        assert!(version_lt("1.0.0", "1.0.1"));
        assert!(version_lt("1.2", "1.10"));
        assert!(!version_lt("1.0.0", "1.0.0"));
        assert!(!version_lt("2.0.0", "1.9.9"));
        assert!(version_lt("1.0.0-beta", "1.0.1"));
    }
}
