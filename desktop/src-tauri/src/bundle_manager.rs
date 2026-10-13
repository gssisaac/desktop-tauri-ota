//! Full-stack OTA bundles: a Next.js standalone server (UI + API) zipped, signed and
//! published to R2 by `pnpm release:ota`. The launcher installs them under
//! `~/Library/Application Support/__APP_IDENTIFIER__/bundles/v<version>/`.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
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
    Downloading { received: u64, total: Option<u64> },
    Installing,
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
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
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
    let dest_canon = dest
        .canonicalize()
        .unwrap_or_else(|_| dest.to_path_buf());

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let name = file.name().to_string();
        let rel = Path::new(&name);
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
        }
    }
    Ok(())
}

fn install_manifest(
    manifest: &OtaManifest,
    on_progress: &dyn Fn(Progress),
) -> Result<Bundle, String> {
    let version_path = version_dir(&manifest.version)?;
    let zip_bytes = download_bytes(&manifest.url, manifest.size, on_progress)?;
    on_progress(Progress::Installing);
    crypto::verify_bundle(&zip_bytes, &manifest.sha256, &manifest.signature)?;

    let stage = staging_dir()?;
    extract_zip_secure(&zip_bytes, &stage)?;
    if !stage.join(SERVER_ENTRY).is_file() {
        fs::remove_dir_all(&stage).ok();
        return Err(format!("bundle v{} has no {SERVER_ENTRY}", manifest.version));
    }

    if version_path.exists() {
        fs::remove_dir_all(&version_path).map_err(|e| format!("clear old install: {e}"))?;
    }
    fs::rename(&stage, &version_path).map_err(|e| e.to_string())?;

    write_active(&manifest.version, &version_path)?;
    log::info!("OTA bundle installed: v{}", manifest.version);
    Ok(Bundle {
        version: manifest.version.clone(),
        dir: version_path,
    })
}

fn active_bundle() -> Option<Bundle> {
    let raw = fs::read_to_string(active_pointer_path().ok()?).ok()?;
    let ptr: ActivePointer = serde_json::from_str(&raw).ok()?;
    let dir = PathBuf::from(ptr.path);
    if dir.join(SERVER_ENTRY).is_file() {
        Some(Bundle {
            version: ptr.version,
            dir,
        })
    } else {
        log::warn!(
            "cached bundle has no {SERVER_ENTRY} ({}); ignoring active pointer",
            dir.display()
        );
        None
    }
}

/// Resolve the bundle to run: install the manifest's release when it differs from the
/// cached one, otherwise (or when offline / on failure) run the cached bundle. The very
/// first launch has no cache and therefore needs the network.
pub fn ensure_bundle(app_version: &str, on_progress: &dyn Fn(Progress)) -> Result<Bundle, String> {
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
        install_manifest(&manifest, on_progress).map(Some)
    });

    match (update, active) {
        (Ok(Some(bundle)), _) => Ok(bundle),
        (Ok(None), Some(active)) => Ok(active),
        (Err(err), Some(active)) => {
            log::warn!("OTA update skipped, running cached v{}: {err}", active.version);
            Ok(active)
        }
        (Err(err), None) => Err(format!("Couldn't download the __APP_NAME__ engine: {err}")),
        (Ok(None), None) => Err("no __APP_NAME__ engine installed".into()),
    }
}

/// Remove every installed bundle except `keep` (old versions, interrupted staging).
pub fn prune_inactive(keep: &Bundle) {
    let Ok(root) = bundles_root() else { return };
    let Ok(entries) = fs::read_dir(&root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if Some(name.as_os_str()) == keep.dir.file_name() {
            continue;
        }
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

#[tauri::command]
pub fn ota_status() -> Result<serde_json::Value, String> {
    let active = active_bundle();
    Ok(serde_json::json!({
        "manifestUrl": MANIFEST_URL,
        "activeVersion": active.as_ref().map(|b| b.version.clone()),
        "activePath": active.as_ref().map(|b| b.dir.display().to_string()),
    }))
}
