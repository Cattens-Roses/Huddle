//! Auto-update helpers.
//!
//! - Windows: differential file updates from the rolling `beta` GitHub release
//!   (tezeusz-style content-addressed blobs + apply script).
//! - Linux: version check against GitHub; installs from the apt repo update via
//!   the package manager (mdsys-style).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[cfg(target_os = "windows")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "windows")]
use std::io::Write;
#[cfg(target_os = "windows")]
use std::process::Command;

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
static PROMPT_REQUESTED: AtomicBool = AtomicBool::new(false);
static DOWNLOAD_POLICY: Mutex<()> = Mutex::new(());
static DOWNLOADS: DownloadGate = DownloadGate::new(true);
static DEFER_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static LAST_PERIODIC: Mutex<Option<Instant>> = Mutex::new(None);
static STAGE_DIR: OnceLock<PathBuf> = OnceLock::new();
static INSTALL_DIR: OnceLock<PathBuf> = OnceLock::new();

struct DownloadGate {
    enabled: AtomicBool,
    generation: AtomicU64,
}
impl DownloadGate {
    const fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            generation: AtomicU64::new(0),
        }
    }
    fn set(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn ticket(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    fn allows(&self, ticket: u64) -> bool {
        self.enabled.load(Ordering::SeqCst) && self.ticket() == ticket
    }
}

pub fn downloads_enabled() -> bool {
    DOWNLOADS.enabled.load(Ordering::SeqCst)
}

pub fn set_downloads_enabled(enabled: bool) {
    let _policy = DOWNLOAD_POLICY.lock().unwrap_or_else(|e| e.into_inner());
    if state() == State::Applying {
        return;
    }
    DOWNLOADS.set(enabled);
    PROMPT_REQUESTED.store(false, Ordering::Relaxed);
    if !enabled {
        PERCENT.store(0, Ordering::Relaxed);
        if matches!(
            state(),
            State::Downloading | State::Ready | State::Available
        ) {
            set_status(
                State::Available,
                "Update downloads are off. You can still check for new versions.",
            );
        }
    } else {
        CHECK_REQUESTED.store(true, Ordering::Relaxed);
    }
}

pub fn take_restart_prompt() -> bool {
    PROMPT_REQUESTED.swap(false, Ordering::Relaxed)
        && state() == State::Ready
        && downloads_enabled()
}

/// The only installation entry point; called after the user confirms a restart.
pub fn apply_ready() {
    #[cfg(target_os = "windows")]
    {
        let _policy = DOWNLOAD_POLICY.lock().unwrap_or_else(|e| e.into_inner());
        if state() == State::Ready && downloads_enabled() && !WORKER_BUSY.load(Ordering::Relaxed) {
            begin_apply_windows();
        }
    }
}

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
        let st = state();
        if !matches!(
            st,
            State::Downloading | State::Checking | State::Ready | State::Applying
        ) {
            set_status(State::Disabled, "Automatic updates are off.");
        }
        return;
    }
    if matches!(
        state(),
        State::Checking | State::Downloading | State::Ready | State::Applying
    ) {
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

pub fn init(auto: bool, downloads: bool) {
    AUTO_ENABLED.store(auto, Ordering::Relaxed);
    STOP.store(false, Ordering::Relaxed);
    WANT_QUIT.store(false, Ordering::Relaxed);
    DOWNLOADS.set(downloads);
    PROMPT_REQUESTED.store(false, Ordering::Relaxed);
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

#[allow(dead_code)]
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

    if !WORKER_BUSY.load(Ordering::Relaxed) && CHECK_REQUESTED.swap(false, Ordering::Relaxed) {
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
    if environment_disabled() {
        return "Updates are disabled by HUDDLE_DISABLE_UPDATE.";
    }
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
    if environment_disabled() {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        dir_writable(install_dir())
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

fn environment_disabled() -> bool {
    std::env::var_os("HUDDLE_DISABLE_UPDATE").is_some_and(|value| !value.is_empty() && value != "0")
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(target_os = "windows")]
fn install_dir() -> &'static Path {
    INSTALL_DIR.get_or_init(exe_dir)
}

fn stage_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Huddle")
        .join("update-stage")
}

#[cfg(target_os = "windows")]
fn stage() -> &'static Path {
    STAGE_DIR.get_or_init(stage_dir)
}

#[cfg(target_os = "windows")]
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
    if environment_disabled() {
        set_status(State::Disabled, unsupported_message());
        return;
    }
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

#[cfg(target_os = "windows")]
fn http_get_long(url: &str, ticket: u64) -> Result<Vec<u8>, String> {
    if !DOWNLOADS.allows(ticket) {
        return Err("Download cancelled.".into());
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    read_download(&mut response, || {
        DOWNLOADS.allows(ticket) && !STOP.load(Ordering::Relaxed)
    })
}

#[cfg(any(target_os = "windows", test))]
fn read_download(
    reader: &mut impl std::io::Read,
    allowed: impl Fn() -> bool,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        if !allowed() {
            return Err("Download cancelled.".into());
        }
        let read = reader.read(&mut chunk).map_err(|e| e.to_string())?;
        if !allowed() {
            return Err("Download cancelled.".into());
        }
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(target_os = "windows")]
fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[cfg(target_os = "windows")]
fn sha256_file(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    Some(sha256_bytes(&data))
}

#[cfg(target_os = "windows")]
#[derive(Deserialize)]
struct Manifest {
    version: String,
    platform: String,
    #[serde(default)]
    #[serde(rename = "baseUrl")]
    base_url: String,
    files: Vec<ManifestFile>,
}

#[cfg(target_os = "windows")]
#[derive(Deserialize)]
struct ManifestFile {
    path: String,
    sha256: String,
    #[serde(default)]
    size: i64,
}

#[cfg(target_os = "windows")]
fn manifest_url() -> String {
    format!(
        "https://github.com/{UPDATE_REPO}/releases/download/{UPDATE_CHANNEL}/update-manifest-windows-x64.json"
    )
}

#[cfg(target_os = "windows")]
fn normalize_rel(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

#[cfg(target_os = "windows")]
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

#[cfg(any(target_os = "windows", test))]
fn prune_stage(root: &Path, wanted: &std::collections::HashSet<PathBuf>) -> Result<(), String> {
    fn visit(
        root: &Path,
        dir: &Path,
        wanted: &std::collections::HashSet<PathBuf>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() {
                return Err(std::io::Error::other(
                    "Unexpected link in update staging directory",
                ));
            }
            if kind.is_dir() {
                visit(root, &path, wanted)?;
            } else if !wanted.contains(path.strip_prefix(root).map_err(std::io::Error::other)?) {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
    fs::create_dir_all(root)
        .and_then(|_| visit(root, root, wanted))
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn worker_windows() {
    let ticket = DOWNLOADS.ticket();
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
        if rel.is_empty()
            || rel.contains("..")
            || file.sha256.len() != 64
            || Path::new(&rel)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
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

    if !DOWNLOADS.allows(ticket) {
        set_status(
            State::Available,
            "An update is available. Update downloads are off.",
        );
        return;
    }

    set_status(
        State::Downloading,
        format!("Downloading update ({} files)…", need.len()),
    );
    // A cancelled download may belong to an older manifest. Never copy stale
    // staged files over already-current local files when a newer update resumes.
    let wanted = need.iter().map(|(rel, _, _)| PathBuf::from(rel)).collect();
    if let Err(error) = prune_stage(stage(), &wanted) {
        set_status(
            State::Error,
            format!("Could not prepare update files: {error}"),
        );
        return;
    }

    let total = need.len().max(1);
    for (index, (rel, sha, _size)) in need.into_iter().enumerate() {
        if STOP.load(Ordering::Relaxed) || !DOWNLOADS.allows(ticket) {
            set_status(
                State::Available,
                "Update download paused. Enable downloads to continue.",
            );
            return;
        }
        let dest = stage().join(&rel);
        if dest.exists() && sha256_file(&dest).is_some_and(|got| got.eq_ignore_ascii_case(&sha)) {
            PERCENT.store((((index + 1) * 100) / total) as i32, Ordering::Relaxed);
            continue;
        }
        let url = format!("{base}b_{sha}");
        let bytes = match http_get_long(&url, ticket) {
            Ok(b) => b,
            Err(err) => {
                if !DOWNLOADS.allows(ticket) {
                    set_status(
                        State::Available,
                        "Update download paused. Enable downloads to continue.",
                    );
                    return;
                }
                set_status(State::Error, format!("Download failed for {rel}: {err}"));
                return;
            }
        };
        if !DOWNLOADS.allows(ticket) {
            set_status(State::Available, "Update download paused.");
            return;
        }
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

    let _policy = DOWNLOAD_POLICY.lock().unwrap_or_else(|e| e.into_inner());
    if !DOWNLOADS.allows(ticket) {
        set_status(State::Available, "Update download paused.");
        return;
    }
    let _ = fs::write(stage().join(".huddle-update-version"), &man.version);
    PERCENT.store(100, Ordering::Relaxed);
    set_status(
        State::Ready,
        format!(
            "Update {} is ready. Restart when you're ready to install it.",
            man.version
        ),
    );
    PROMPT_REQUESTED.store(true, Ordering::Relaxed);
}

#[cfg(target_os = "windows")]
fn win_path(path: &Path) -> String {
    // Trailing backslash + quotes breaks cmd.exe argument parsing ("C:\foo\" → open quote).
    let mut text = path.to_string_lossy().replace('/', "\\");
    while text.ends_with('\\') && !text.ends_with(":\\") {
        text.pop();
    }
    text
}

#[cfg(target_os = "windows")]
fn begin_apply_windows() {
    use std::os::windows::process::CommandExt;

    set_status(State::Applying, "Applying update…");
    let pid = std::process::id().to_string();
    let script = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Huddle")
        .join("apply-update.cmd");
    if let Some(parent) = script.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Keep the apply helper self-contained: never rely on `start "" …` from Rust,
    // because empty-title quoting collapses and the PID becomes the "command".
    let body = "@echo off\r\n\
         setlocal EnableExtensions\r\n\
         set \"PID=%~1\"\r\n\
         set \"STAGE=%~2\"\r\n\
         set \"DEST=%~3\"\r\n\
         set \"LOG=%TEMP%\\huddle-update-apply.log\"\r\n\
         echo apply start %DATE% %TIME% > \"%LOG%\"\r\n\
         echo PID=%PID%>> \"%LOG%\"\r\n\
         echo STAGE=%STAGE%>> \"%LOG%\"\r\n\
         echo DEST=%DEST%>> \"%LOG%\"\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq %PID%\" 2>nul | find \"%PID%\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           timeout /t 1 /nobreak >nul\r\n\
           goto wait\r\n\
         )\r\n\
         timeout /t 2 /nobreak >nul\r\n\
         if not exist \"%STAGE%\\Huddle.exe\" (\r\n\
           echo missing STAGE or Huddle.exe>> \"%LOG%\"\r\n\
           exit /b 1\r\n\
         )\r\n\
         robocopy \"%STAGE%\" \"%DEST%\" /E /IS /IT /R:5 /W:1 /NFL /NDL /NJH /NJS /NC /NS /XF .huddle-update-version >> \"%LOG%\" 2>&1\r\n\
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
         start \"Huddle\" /D \"%DEST%\" \"%DEST%\\Huddle.exe\"\r\n\
         del \"%~f0\" 2>nul\r\n\
         exit /b 0\r\n";
    if fs::write(&script, body).is_err() {
        set_status(State::Error, "Could not write apply script.");
        return;
    }
    let stage = win_path(stage());
    let dest = win_path(install_dir());
    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
    // Spawn the .cmd directly so Rust's Windows Command path quotes %1/%2/%3 correctly
    // (unlike `start "" …`, which ate the empty title and treated the PID as a program).
    const FLAGS: u32 = 0x00000008 | 0x00000200 | 0x08000000;
    let ok = Command::new(&script)
        .arg(&pid)
        .arg(&stage)
        .arg(&dest)
        .creation_flags(FLAGS)
        .spawn()
        .is_ok();
    if !ok {
        set_status(State::Error, "Could not start apply script.");
        return;
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

#[cfg(not(target_os = "windows"))]
pub fn apt_setup_url() -> &'static str {
    "https://cattens-roses.github.io/Huddle/"
}

pub fn releases_url() -> String {
    format!("https://github.com/{UPDATE_REPO}/releases/tag/{UPDATE_CHANNEL}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn disabling_downloads_cancels_old_requests_even_after_reenabling() {
        let gate = DownloadGate::new(true);
        let first = gate.ticket();
        assert!(gate.allows(first));
        gate.set(false);
        assert!(!gate.allows(first));
        gate.set(true);
        assert!(!gate.allows(first));
        assert!(gate.allows(gate.ticket()));
    }

    #[test]
    fn disabled_download_does_not_read_the_response_body() {
        let mut body = Cursor::new(vec![1u8; 128 * 1024]);
        assert!(read_download(&mut body, || false).is_err());
        assert_eq!(body.position(), 0);
    }

    #[test]
    fn disabling_a_running_download_stops_before_the_next_chunk() {
        struct Body<'a> {
            gate: &'a DownloadGate,
            reads: usize,
        }
        impl std::io::Read for Body<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                out.fill(7);
                self.gate.set(false);
                Ok(out.len())
            }
        }
        let gate = DownloadGate::new(true);
        let ticket = gate.ticket();
        let mut body = Body {
            gate: &gate,
            reads: 0,
        };
        assert!(read_download(&mut body, || gate.allows(ticket)).is_err());
        assert_eq!(body.reads, 1);
    }

    #[test]
    fn enabled_download_preserves_the_complete_response() {
        let bytes = vec![42; 150_000];
        assert_eq!(
            read_download(&mut Cursor::new(&bytes), || true).unwrap(),
            bytes
        );
    }

    #[test]
    fn resuming_an_update_removes_stale_files_from_previous_manifests() {
        let base = std::env::temp_dir();
        let root = base.join(format!(
            "huddle-updater-stage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("Huddle.exe"), b"needed").unwrap();
        fs::write(root.join("nested/old.dll"), b"stale").unwrap();
        fs::write(root.join(".huddle-update-version"), b"old").unwrap();
        prune_stage(&root, &[PathBuf::from("Huddle.exe")].into()).unwrap();
        assert_eq!(fs::read(root.join("Huddle.exe")).unwrap(), b"needed");
        assert!(!root.join("nested/old.dll").exists());
        assert!(!root.join(".huddle-update-version").exists());
        assert!(root.starts_with(&base));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ready_update_does_not_restart_without_confirmation_or_repeat_the_prompt() {
        STATE.store(State::Ready as u8, Ordering::Relaxed);
        AUTO_ENABLED.store(true, Ordering::Relaxed);
        DOWNLOADS.set(true);
        PROMPT_REQUESTED.store(true, Ordering::Relaxed);
        WANT_QUIT.store(false, Ordering::Relaxed);
        tick();
        assert_eq!(state() as u8, State::Ready as u8);
        assert!(!wants_quit_for_apply());
        assert!(take_restart_prompt());
        assert!(!take_restart_prompt());
        // Choosing Later only dismisses the prompt. Periodic ticks must leave
        // the staged update ready until the explicit apply_ready entry point.
        for _ in 0..3 {
            tick();
        }
        assert_eq!(state() as u8, State::Ready as u8);
        assert!(!wants_quit_for_apply());
        STATE.store(State::Idle as u8, Ordering::Relaxed);
    }
}
