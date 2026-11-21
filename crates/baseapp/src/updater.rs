//! Auto-update helpers.
//!
//! - Windows: differential file updates from the rolling `beta` GitHub release
//!   (tezeusz-style content-addressed blobs + apply script).
//! - Linux: version check against GitHub; installs from the apt repo update via
//!   the package manager (mdsys-style).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const UPDATE_REPO: &str = "Cattens-Roses/Huddle";
const UPDATE_CHANNEL: &str = "beta";
const USER_AGENT: &str = concat!("Huddle/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Idle = 0,
    Checking = 1,
    UpToDate = 2,
    Downloading = 3,
    Ready = 4,
    Applying = 5,
    Disabled = 6,
    Error = 7,
    Available = 8,
}

impl State {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Checking,
            2 => Self::UpToDate,
            3 => Self::Downloading,
            4 => Self::Ready,
            5 => Self::Applying,
            6 => Self::Disabled,
            7 => Self::Error,
            8 => Self::Available,
            _ => Self::Idle,
        }
    }
}

static STATE: AtomicU8 = AtomicU8::new(State::Idle as u8);
static PERCENT: AtomicI32 = AtomicI32::new(0);
static WANT_QUIT: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static CHECK_REQUESTED: AtomicBool = AtomicBool::new(false);
static WORKER_BUSY: AtomicBool = AtomicBool::new(false);
static AUTO_ENABLED: AtomicBool = AtomicBool::new(true);
static STATUS: Mutex<String> = Mutex::new(String::new());
static REMOTE: Mutex<String> = Mutex::new(String::new());
static READY_SINCE: Mutex<Option<Instant>> = Mutex::new(None);
static DEFER_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static LAST_PERIODIC: Mutex<Option<Instant>> = Mutex::new(None);
static STAGE_DIR: OnceLock<PathBuf> = OnceLock::new();
static INSTALL_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn current_version() -> &'static str {
    option_env!("HUDDLE_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

pub fn state() -> State {
    State::from_u8(STATE.load(Ordering::Relaxed))
}

pub fn status_text() -> String {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

pub fn remote_version() -> String {
    REMOTE.lock().map(|s| s.clone()).unwrap_or_default()
}

pub fn download_percent() -> i32 {
    PERCENT.load(Ordering::Relaxed)
}

pub fn wants_quit_for_apply() -> bool {
    WANT_QUIT.load(Ordering::Relaxed)
}

pub fn auto_enabled() -> bool {
    AUTO_ENABLED.load(Ordering::Relaxed)
}

pub fn set_auto_enabled(on: bool) {
    AUTO_ENABLED.store(on, Ordering::Relaxed);
    if !on {
        CHECK_REQUESTED.store(false, Ordering::Relaxed);
        WANT_QUIT.store(false, Ordering::Relaxed);
        *READY_SINCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let st = state();
        if !matches!(st, State::Downloading | State::Checking | State::Applying) {
            set_status(State::Disabled, "Automatic updates are off.");
        }
        return;
    }
    if updates_supported() {
        set_status(State::Idle, idle_message());
        *DEFER_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        CHECK_REQUESTED.store(true, Ordering::Relaxed);
    } else {
        set_status(State::Disabled, unsupported_message());
    }
}

pub fn check_now() {
    if !updates_supported() {
        set_status(State::Disabled, unsupported_message());
        return;
    }
    *DEFER_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if state() == State::Disabled {
        set_status(State::Idle, "Checking for updates…");
    }
    CHECK_REQUESTED.store(true, Ordering::Relaxed);
}

pub fn init(auto: bool) {
    AUTO_ENABLED.store(auto, Ordering::Relaxed);
    STOP.store(false, Ordering::Relaxed);
    WANT_QUIT.store(false, Ordering::Relaxed);
    let _ = INSTALL_DIR.set(exe_dir());
    let _ = STAGE_DIR.set(stage_dir());

    if std::env::var_os("HUDDLE_DISABLE_UPDATE").is_some_and(|v| !v.is_empty() && v != "0") {
        set_status(
            State::Disabled,
            "Updates are disabled by HUDDLE_DISABLE_UPDATE.",
        );
        return;
    }
    if !auto {
        set_status(State::Disabled, "Automatic updates are off.");
        return;
    }
    if !updates_supported() {
        set_status(State::Disabled, unsupported_message());
        return;
    }
    set_status(State::Idle, idle_message());
    *DEFER_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Instant::now() + Duration::from_secs(6));
}

pub fn shutdown() {
    STOP.store(true, Ordering::Relaxed);
}

/// Call from the UI thread periodically.
pub fn tick() {
    if state() == State::Applying {
        return;
    }

    let now = Instant::now();
    {
        let mut defer = DEFER_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
        if defer.is_some_and(|at| now >= at) {
            *defer = None;
            if auto_enabled() {
                CHECK_REQUESTED.store(true, Ordering::Relaxed);
            }
        }
    }

    if CHECK_REQUESTED.swap(false, Ordering::Relaxed) {
        let st = state();
        if !matches!(
            st,
            State::Downloading | State::Checking | State::Ready | State::Applying
        ) {
            enqueue_check();
        }
    }

    if state() == State::Disabled || !auto_enabled() {
        return;
    }

    {
        let mut last = LAST_PERIODIC.lock().unwrap_or_else(|e| e.into_inner());
        let st = state();
        if matches!(
            st,
            State::Idle | State::UpToDate | State::Error | State::Available
        ) && !WORKER_BUSY.load(Ordering::Relaxed)
        {
            match *last {
                None => *last = Some(now),
                Some(prev) if now.duration_since(prev) > Duration::from_secs(6 * 3600) => {
                    *last = Some(now);
                    enqueue_check();
                }
                _ => {}
            }
        }
    }

    #[cfg(target_os = "windows")]
    if state() == State::Ready {
        let mut since = READY_SINCE.lock().unwrap_or_else(|e| e.into_inner());
        if since.is_none() {
            *since = Some(now);
        }
        if since.is_some_and(|at| now.duration_since(at) >= Duration::from_millis(1500)) {
            begin_apply_windows();
        }
    }
}

fn set_status(state: State, text: impl Into<String>) {
    STATE.store(state as u8, Ordering::Relaxed);
    if let Ok(mut status) = STATUS.lock() {
        *status = text.into();
    }
}

fn idle_message() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Updates check the rolling beta channel automatically."
    }
    #[cfg(not(target_os = "windows"))]
    {
        "Package updates come from the Huddle apt repository."
    }
}

fn unsupported_message() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Install directory is not writable. Reinstall with the per-user setup."
    }
    #[cfg(not(target_os = "windows"))]
    {
        "Use the apt repository for automatic Linux updates."
    }
}

fn updates_supported() -> bool {
    #[cfg(target_os = "windows")]
    {
        dir_writable(install_dir())
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn install_dir() -> &'static Path {
    INSTALL_DIR.get_or_init(exe_dir)
}

fn stage_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Huddle")
        .join("update-stage")
}

fn stage() -> &'static Path {
    STAGE_DIR.get_or_init(stage_dir)
}

fn dir_writable(dir: &Path) -> bool {
    if !dir.exists() {
        return false;
    }
    let probe = dir.join(".huddle-write-test");
    match fs::File::create(&probe).and_then(|mut f| f.write_all(b"ok")) {
        Ok(()) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn enqueue_check() {
    if WORKER_BUSY.swap(true, Ordering::Relaxed) {
        return;
    }
    thread::spawn(|| {
        #[cfg(target_os = "windows")]
        worker_windows();
        #[cfg(not(target_os = "windows"))]
        worker_linux();
        WORKER_BUSY.store(false, Ordering::Relaxed);
    });
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response
        .bytes()
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

fn http_get_long(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response
        .bytes()
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn sha256_file(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    Some(sha256_bytes(&data))
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    platform: String,
    #[serde(default)]
    #[serde(rename = "baseUrl")]
    base_url: String,
    files: Vec<ManifestFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    path: String,
    sha256: String,
    #[serde(default)]
    size: i64,
}

fn manifest_url() -> String {
    format!(
        "https://github.com/{UPDATE_REPO}/releases/download/{UPDATE_CHANNEL}/update-manifest-windows-x64.json"
    )
}

fn normalize_rel(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path)
        .or_else(|_| {
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)
        })
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
fn worker_windows() {
    if STOP.load(Ordering::Relaxed) {
        return;
    }
    set_status(State::Checking, "Checking for updates…");
    PERCENT.store(0, Ordering::Relaxed);

    let body = match http_get(&manifest_url()) {
        Ok(bytes) => bytes,
        Err(err) => {
            set_status(State::Error, format!("Update check failed: {err}"));
            return;
        }
    };
    let man: Manifest = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(err) => {
            set_status(State::Error, format!("Bad update manifest: {err}"));
            return;
        }
    };
    if man.platform != "windows-x64" {
        set_status(State::Error, "Update manifest platform mismatch.");
        return;
    }
    if let Ok(mut remote) = REMOTE.lock() {
        *remote = man.version.clone();
    }
    if !man.version.is_empty() && man.version == current_version() {
        set_status(State::UpToDate, "You're up to date.");
        return;
    }

    let base = if man.base_url.is_empty() {
        format!("https://github.com/{UPDATE_REPO}/releases/download/{UPDATE_CHANNEL}/")
    } else {
        man.base_url
    };

    let mut need = Vec::new();
    for file in &man.files {
        let rel = normalize_rel(&file.path);
        if rel.is_empty() || rel.contains("..") || file.sha256.len() != 64 {
            continue;
        }
        let local = install_dir().join(&rel);
        match sha256_file(&local) {
            Some(sha) if sha.eq_ignore_ascii_case(&file.sha256) => {}
            _ => need.push((rel, file.sha256.to_ascii_lowercase(), file.size)),
        }
    }

    if need.is_empty() {
        set_status(State::UpToDate, "You're up to date.");
        return;
    }

    set_status(
        State::Downloading,
        format!("Downloading update ({} files)…", need.len()),
    );
    let _ = fs::create_dir_all(stage());

    let total = need.len().max(1);
    for (index, (rel, sha, _size)) in need.into_iter().enumerate() {
        if STOP.load(Ordering::Relaxed) {
            return;
        }
        let dest = stage().join(&rel);
        if dest.exists() && sha256_file(&dest).is_some_and(|got| got.eq_ignore_ascii_case(&sha)) {
            PERCENT.store((((index + 1) * 100) / total) as i32, Ordering::Relaxed);
            continue;
        }
        let url = format!("{base}b_{sha}");
        let bytes = match http_get_long(&url) {
            Ok(b) => b,
            Err(err) => {
                set_status(State::Error, format!("Download failed for {rel}: {err}"));
                return;
            }
        };
        if !sha256_bytes(&bytes).eq_ignore_ascii_case(&sha) {
            set_status(State::Error, format!("Checksum failed for {rel}"));
            return;
        }
        if let Err(err) = write_atomic(&dest, &bytes) {
            set_status(State::Error, format!("Could not stage {rel}: {err}"));
            return;
        }
        PERCENT.store((((index + 1) * 100) / total) as i32, Ordering::Relaxed);
        set_status(
            State::Downloading,
            format!("Downloading update {}/{}…", index + 1, total),
        );
    }

    let _ = fs::write(stage().join(".huddle-update-version"), &man.version);
    PERCENT.store(100, Ordering::Relaxed);
    *READY_SINCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_status(
        State::Ready,
        format!("Update {} ready. Restarting…", man.version),
    );
}

#[cfg(target_os = "windows")]
fn begin_apply_windows() {
    set_status(State::Applying, "Applying update…");
    let pid = std::process::id().to_string();
    let script = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Huddle")
        .join("apply-update.cmd");
    if let Some(parent) = script.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body = format!(
        "@echo off\r\n\
         setlocal EnableExtensions\r\n\
         set \"PID=%~1\"\r\n\
         set \"STAGE=%~2\"\r\n\
         set \"DEST=%~3\"\r\n\
         set \"LOG=%TEMP%\\huddle-update-apply.log\"\r\n\
         echo apply start %DATE% %TIME% PID=%PID% > \"%LOG%\"\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq %PID%\" 2>nul | find \"%PID%\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           timeout /t 1 /nobreak >nul\r\n\
           goto wait\r\n\
         )\r\n\
         timeout /t 2 /nobreak >nul\r\n\
         if not exist \"%STAGE%\" (\r\n\
           echo missing STAGE>> \"%LOG%\"\r\n\
           exit /b 1\r\n\
         )\r\n\
         robocopy \"%STAGE%\" \"%DEST%\" /E /IS /IT /R:3 /W:1 /NFL /NDL /NJH /NJS /NC /NS /XF .huddle-update-version >> \"%LOG%\" 2>&1\r\n\
         if errorlevel 8 (\r\n\
           echo copy failed>> \"%LOG%\"\r\n\
           exit /b 1\r\n\
         )\r\n\
         if not exist \"%DEST%\\Huddle.exe\" (\r\n\
           echo missing Huddle.exe after copy>> \"%LOG%\"\r\n\
           exit /b 1\r\n\
         )\r\n\
         rmdir /S /Q \"%STAGE%\" 2>nul\r\n\
         echo ok, restarting>> \"%LOG%\"\r\n\
         start \"\" /D \"%DEST%\" \"%DEST%\\Huddle.exe\"\r\n\
         del \"%~f0\" 2>nul\r\n\
         exit /b 0\r\n"
    );
    if fs::write(&script, body).is_err() {
        set_status(State::Error, "Could not write apply script.");
        return;
    }
    let stage = stage().display().to_string();
    let dest = install_dir().display().to_string();
    let params = format!("{pid} \"{stage}\" \"{dest}\"");
    let ok = Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            "/MIN",
            script.to_str().unwrap_or(""),
            &params,
        ])
        .spawn()
        .is_ok();
    if !ok {
        // Fallback: Shell-less spawn of the script itself.
        let ok = Command::new(&script)
            .args([&pid, &stage, &dest])
            .spawn()
            .is_ok();
        if !ok {
            set_status(State::Error, "Could not start apply script.");
            return;
        }
    }
    WANT_QUIT.store(true, Ordering::Relaxed);
}

#[cfg(not(target_os = "windows"))]
fn worker_linux() {
    if STOP.load(Ordering::Relaxed) {
        return;
    }
    set_status(State::Checking, "Checking for package updates…");
    let url = format!("https://api.github.com/repos/{UPDATE_REPO}/releases/tags/{UPDATE_CHANNEL}");
    let body = match http_get(&url) {
        Ok(bytes) => bytes,
        Err(err) => {
            set_status(State::Error, format!("Update check failed: {err}"));
            return;
        }
    };
    #[derive(Deserialize)]
    struct Release {
        name: Option<String>,
        body: Option<String>,
        #[serde(default)]
        tag_name: String,
    }
    let release: Release = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(err) => {
            set_status(State::Error, format!("Bad release metadata: {err}"));
            return;
        }
    };
    let remote = release
        .body
        .as_deref()
        .and_then(|body| {
            body.lines()
                .find_map(|line| line.strip_prefix("Package version:"))
                .map(str::trim)
                .map(str::to_owned)
        })
        .or_else(|| release.name.clone())
        .unwrap_or(release.tag_name);
    if let Ok(mut remote_slot) = REMOTE.lock() {
        *remote_slot = remote.clone();
    }
    if remote == current_version() || remote == UPDATE_CHANNEL {
        set_status(
            State::UpToDate,
            "You're up to date. New packages arrive through apt.",
        );
        return;
    }
    set_status(
        State::Available,
        format!("Update {remote} is available. Run: sudo apt update && sudo apt upgrade"),
    );
}

pub fn apt_setup_url() -> &'static str {
    "https://cattens-roses.github.io/Huddle/"
}

pub fn releases_url() -> String {
    format!("https://github.com/{UPDATE_REPO}/releases/tag/{UPDATE_CHANNEL}")
}
