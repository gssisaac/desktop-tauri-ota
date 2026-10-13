//! Runs the active full-stack bundle (`node server.js`) on 127.0.0.1:__APP_PORT_PROD__, which serves
//! both the UI and `/api/*` to the webview.

use std::fs::{self, File};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::bundle_manager::{self, Bundle};

/// Must match `capabilities/default.json` remote URL and app dev/prod port docs.
pub const SERVER_PORT: u16 = __APP_PORT_PROD__;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1500);
/// How long a process holding the port gets after each signal before escalating.
const PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(3);
const PATH_SEPARATOR: &str = if cfg!(windows) { ";" } else { ":" };
/// Keeps console children (`node.exe`, `netstat`, `taskkill`) from opening a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runs `server.js` (passed as argv[1]) and exits once the launcher's stdin pipe closes,
/// so a crashed or force-quit shell never leaves an orphaned server holding the port.
const PARENT_WATCHDOG: &str = "process.stdin.on('end',()=>process.exit(0));\
process.stdin.on('error',()=>process.exit(0));\
process.stdin.resume();\
require(process.argv[1]);";

static SERVER: Mutex<Option<Child>> = Mutex::new(None);

#[derive(Deserialize)]
struct HealthEnvelope {
    data: Health,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    app: String,
    bundle_version: Option<String>,
}

pub fn origin() -> String {
    format!("http://127.0.0.1:{SERVER_PORT}")
}

fn port_in_use() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], SERVER_PORT));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

#[cfg(windows)]
fn hidden_command(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// PIDs listening on the port on any address: a stale __APP_NAME__ server, `pnpm start`, anything.
#[cfg(windows)]
fn port_listeners() -> Vec<i32> {
    let Ok(output) = hidden_command("netstat").arg("-ano").output() else {
        return Vec::new();
    };
    let own = std::process::id() as i32;
    let local_suffix = format!(":{SERVER_PORT}");
    // `TCP  127.0.0.1:__APP_PORT_PROD__  0.0.0.0:0  LISTENING  1234`; the state column is localized, but
    // only listening sockets have a `:0` foreign port.
    let mut pids: Vec<i32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| match line.split_whitespace().collect::<Vec<_>>()[..] {
            [proto, local, foreign, _, pid]
                if proto.starts_with("TCP")
                    && local.ends_with(&local_suffix)
                    && foreign.ends_with(":0") =>
            {
                pid.parse().ok()
            }
            _ => None,
        })
        .filter(|pid| *pid != own)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// PIDs listening on the port on any address: a stale __APP_NAME__ server, `pnpm start`, anything.
#[cfg(unix)]
fn port_listeners() -> Vec<i32> {
    let Ok(output) = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-t", "-sTCP:LISTEN"])
        .arg(format!("-iTCP:{SERVER_PORT}"))
        .output()
    else {
        return Vec::new();
    };
    let own = std::process::id() as i32;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid != own)
        .collect()
}

fn wait_port_released(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if port_listeners().is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn stop_pid(pid: i32, force: bool) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: kill(2) only signals the process; a stale pid just returns ESRCH.
    unsafe { libc::kill(pid, signal) };
}

#[cfg(windows)]
fn stop_pid(pid: i32, force: bool) {
    let mut command = hidden_command("taskkill");
    command.args(["/PID", &pid.to_string(), "/T"]);
    if force {
        command.arg("/F");
    }
    let _ = command.output();
}

/// The launcher owns the port: stop whatever listens on it (SIGTERM, then SIGKILL; on Windows
/// `taskkill`, then `taskkill /F`) so the bundle's server always starts fresh on the bundled Node.
fn free_port() -> Result<(), String> {
    for force in [false, true] {
        let pids = port_listeners();
        if pids.is_empty() {
            break;
        }
        log::warn!("port {SERVER_PORT} is held by pid {pids:?}; stopping it (force: {force})");
        for pid in pids {
            stop_pid(pid, force);
        }
        if wait_port_released(PORT_RELEASE_TIMEOUT) {
            break;
        }
    }
    if port_in_use() {
        return Err(format!(
            "Port {SERVER_PORT} is held by another process (pid {:?}) that __APP_NAME__ couldn't stop.",
            port_listeners()
        ));
    }
    Ok(())
}

/// `Some` only when a __APP_NAME__ server answers on the port.
fn probe() -> Option<Health> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HEALTH_TIMEOUT)
        .no_proxy()
        .build()
        .ok()?;
    let resp = client
        .get(format!("{}/api/health", origin()))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<HealthEnvelope>()
        .ok()
        .map(|envelope| envelope.data)
        .filter(|health| health.app == "__APP_SLUG__")
}

/// The Node.js runtime shipped as a Tauri sidecar (`bundle.externalBin`), so bundles always
/// run on the Node ABI they were built for, whatever the host has installed.
fn find_node_binary() -> PathBuf {
    // Release (`__APP_NAME__.app/Contents/MacOS/node`, `node.exe` beside `__APP_NAME__.exe`) and
    // `tauri dev` (`target/debug/node`): Tauri places the sidecar next to the launcher executable.
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(format!("node{exe_suffix}"))))
        .filter(|path| path.is_file())
    {
        return bundled;
    }

    // Debug builds run before Tauri copied the sidecar: `scripts/prepare-node-sidecar.mjs` output.
    #[cfg(debug_assertions)]
    {
        let triple = if cfg!(windows) {
            "x86_64-pc-windows-msvc"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        };
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!("node-{triple}{exe_suffix}"));
        if dev.is_file() {
            return dev;
        }
    }

    log::warn!("bundled Node.js runtime not found; falling back to `node` on PATH");
    PathBuf::from("node")
}

/// True when `path` lives in `Something.app/Contents/MacOS/`. Launch Services treats those
/// binaries as extra GUI executables of the app, which is what makes `node` bounce in the Dock.
fn is_app_macos_sidecar(path: &Path) -> bool {
    let macos = path.parent();
    let contents = macos.and_then(|p| p.parent());
    let app = contents.and_then(|p| p.parent());
    matches!(
        (
            macos.and_then(|p| p.file_name()).and_then(|n| n.to_str()),
            contents.and_then(|p| p.file_name()).and_then(|n| n.to_str()),
            app.and_then(|p| p.file_name()).and_then(|n| n.to_str()),
        ),
        (Some("MacOS"), Some("Contents"), Some(name)) if name.ends_with(".app")
    )
}

/// Exec Node from Application Support, not `Contents/MacOS/node`, so it is not a bundle extra.
fn node_for_spawn() -> Result<PathBuf, String> {
    let bundled = find_node_binary();
    if !is_app_macos_sidecar(&bundled) {
        return Ok(bundled);
    }

    let dest_dir = bundle_manager::app_data_dir()?.join("runtime");
    fs::create_dir_all(&dest_dir).map_err(|e| format!("create runtime dir: {e}"))?;
    let dest = dest_dir.join("node");
    let stale = match (fs::metadata(&bundled), fs::metadata(&dest)) {
        (Ok(src), Ok(dst)) => src.len() != dst.len(),
        _ => true,
    };
    if stale {
        let partial = dest_dir.join("node.partial");
        fs::copy(&bundled, &partial).map_err(|e| format!("copy Node runtime: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&partial, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("chmod Node runtime: {e}"))?;
        }
        fs::rename(&partial, &dest).map_err(|e| format!("install Node runtime: {e}"))?;
        log::info!("copied bundled Node out of the app bundle → {}", dest.display());
    }
    Ok(dest)
}

/// Puts the runtime's directory first so anything the server runs as `node` uses it too.
fn server_path(node: &Path) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    match node.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        Some(dir) => format!("{}{PATH_SEPARATOR}{inherited}", dir.display()),
        None => inherited,
    }
}

fn server_log_path() -> Result<PathBuf, String> {
    let dir = bundle_manager::app_data_dir()?.join("logs");
    fs::create_dir_all(&dir).map_err(|e| format!("create logs dir: {e}"))?;
    Ok(dir.join("server.log"))
}

fn log_tail(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    if tail.trim().is_empty() {
        format!("(no server output; see {})", path.display())
    } else {
        format!("{tail}\n\n(full log: {})", path.display())
    }
}

/// Start the bundle's server and block until `/api/health` answers for this version.
pub fn start(bundle: &Bundle) -> Result<(), String> {
    stop();
    free_port()?;

    let node = node_for_spawn()?;
    let home = bundle_manager::app_data_dir()?;
    let log_path = server_log_path()?;
    let log = File::create(&log_path).map_err(|e| format!("create server log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("clone server log: {e}"))?;

    log::info!(
        "starting __APP_NAME__ server v{}: {} {} (log {})",
        bundle.version,
        node.display(),
        bundle.server_entry().display(),
        log_path.display()
    );

    // Inherited bundle env makes Launch Services treat Node as a second GUI app (Dock bounce).
    let mut command = Command::new(&node);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command
        .env_remove("__CFBundleIdentifier")
        .env_remove("XPC_SERVICE_NAME")
        .env_remove("__CF_USER_TEXT_ENCODING")
        .arg("--eval")
        .arg(PARENT_WATCHDOG)
        .arg(bundle.server_entry())
        .current_dir(&bundle.dir)
        .env("PATH", server_path(&node))
        .env("HOSTNAME", "127.0.0.1")
        .env("PORT", SERVER_PORT.to_string())
        .env("NODE_ENV", "production")
        .env("__APP_SLUG_UPPER___HOME", &home)
        .env("__APP_SLUG_UPPER___BUNDLE_VERSION", &bundle.version)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|e| {
            format!(
                "Couldn't run the bundled Node.js runtime ({}): {e}. Reinstall __APP_NAME__ from __APP_DOMAIN__.",
                node.display()
            )
        })?;

    *SERVER.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let exited = SERVER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        if let Some(status) = exited {
            stop();
            return Err(format!(
                "__APP_NAME__ server exited during startup ({status}).\n\n{}",
                log_tail(&log_path)
            ));
        }
        if probe().is_some_and(|h| h.bundle_version.as_deref() == Some(bundle.version.as_str())) {
            log::info!("__APP_NAME__ server listening on {}", origin());
            return Ok(());
        }
        if Instant::now() >= deadline {
            stop();
            return Err(format!(
                "__APP_NAME__ server did not respond within {}s.\n\n{}",
                STARTUP_TIMEOUT.as_secs(),
                log_tail(&log_path)
            ));
        }
        thread::sleep(Duration::from_millis(150));
    }
}

/// Close the watchdog pipe so the server exits on its own, then kill it if it lingers.
pub fn stop() {
    let Some(mut child) = SERVER.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    drop(child.stdin.take());
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
