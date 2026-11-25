//! Cross-platform attachment downloads. Incomplete files stay temporary and are
//! published without overwriting existing files only after decryption succeeds.
use anyhow::{Result, anyhow, bail};
use matrix_sdk::{
    Client,
    ruma::{api::Metadata, events::room::MediaSource},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Cancel {
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}
impl Cancel {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.wake.notify_one();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    async fn cancelled(&self) {
        if !self.is_cancelled() {
            self.wake.notified().await;
        }
    }
}

#[derive(Clone)]
pub enum Progress {
    Connecting,
    Receiving {
        received: u64,
        total: Option<u64>,
        speed: f64,
        eta: Option<Duration>,
    },
    Saving,
    Complete(PathBuf),
    Cancelled,
    Failed(String),
}
impl Progress {
    pub fn active(&self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Receiving { .. } | Self::Saving
        )
    }
    pub fn label(&self) -> String {
        match self {
            Self::Connecting => "Connecting…".into(),
            Self::Saving => "Verifying and saving…".into(),
            Self::Cancelled => "Cancelled".into(),
            Self::Failed(error) => format!("Failed: {error}"),
            Self::Complete(path) => format!("Saved to {}", path.display()),
            Self::Receiving {
                received,
                total,
                speed,
                eta,
            } => format!(
                "{}{} · {}/s · {}",
                bytes(*received),
                total
                    .map(|total| format!(" / {}", bytes(total)))
                    .unwrap_or_default(),
                bytes(*speed as u64),
                eta.map(|eta| format!("~{}:{:02} left", eta.as_secs() / 60, eta.as_secs() % 60))
                    .unwrap_or_else(|| "Calculating time…".into())
            ),
        }
    }
    pub fn fraction(&self) -> Option<f32> {
        match self {
            Self::Receiving {
                received,
                total: Some(total),
                ..
            } if *total > 0 => Some((*received as f64 / *total as f64).clamp(0., 1.) as f32),
            Self::Complete(_) => Some(1.),
            _ => None,
        }
    }
}

pub fn bytes(value: u64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", value as f64 / (1024. * 1024. * 1024.))
    } else if value >= 1024 * 1024 {
        format!("{:.1} MB", value as f64 / (1024. * 1024.))
    } else if value >= 1024 {
        format!("{:.0} KB", value as f64 / 1024.)
    } else {
        format!("{value} B")
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    directory: Option<PathBuf>,
}
fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("Huddle").join("downloads.json"))
}
pub fn default_directory() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(std::env::temp_dir)
        .join("huddle-client")
}
pub fn directory() -> PathBuf {
    settings_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .and_then(|settings| settings.directory)
        .filter(|directory| directory.is_absolute())
        .unwrap_or_else(default_directory)
}
pub fn set_directory(directory: Option<PathBuf>) -> Result<()> {
    if let Some(directory) = &directory {
        anyhow::ensure!(directory.is_absolute(), "Choose an absolute folder path");
    }
    let path = settings_path().ok_or_else(|| anyhow!("No configuration directory"))?;
    save_settings(&path, &Settings { directory })
}

fn save_settings(path: &Path, settings: &Settings) -> Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow!("Invalid settings path"))?,
    )?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    file.write_all(&serde_json::to_vec(settings)?)?;
    file.persist(path)?;
    Ok(())
}

pub fn filename(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or("download");
    let mut result: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    result = result.trim_matches([' ', '.']).to_owned();
    if result.is_empty() {
        result = "download".into();
    }
    let stem = result
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        result.insert(0, '_');
    }
    if result.len() > 200 {
        let extension = Path::new(&result)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{s}"))
            .unwrap_or_default();
        let extension = if extension.len() < 40 {
            extension
        } else {
            String::new()
        };
        let mut end = 200 - extension.len();
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
        result.push_str(&extension);
    }
    result
}

fn publish(
    mut file: tempfile::NamedTempFile,
    directory: &Path,
    name: &str,
    cancel: &Cancel,
) -> Result<PathBuf> {
    file.as_file_mut().sync_all()?;
    let name = filename(name);
    let stem = Path::new(&name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let extension = Path::new(&name)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for index in 0..10_000 {
        anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
        let path = directory.join(if index == 0 {
            name.clone()
        } else {
            format!("{stem} ({index}){extension}")
        });
        match file.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                file = error.file
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    bail!("Too many files with this name")
}

struct CancellableReader<R> {
    inner: R,
    cancel: Arc<Cancel>,
}
impl<R: Read> Read for CancellableReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::other("Cancelled"));
        }
        self.inner.read(bytes)
    }
}

pub async fn receive(
    client: &Client,
    source: MediaSource,
    directory: PathBuf,
    name: String,
    expected: Option<u64>,
    cancel: Arc<Cancel>,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<PathBuf> {
    anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
    std::fs::create_dir_all(&directory)?;
    let mut file = tempfile::Builder::new()
        .prefix(".huddle-")
        .suffix(".part")
        .tempfile_in(&directory)?;
    let uri = match &source {
        MediaSource::Plain(uri) => uri,
        MediaSource::Encrypted(file) => &file.url,
    };
    let (server, media_id) = uri.parts()?;
    let versions = tokio::select! { biased;
        _ = cancel.cancelled() => bail!("Cancelled"),
        versions = client.supported_versions() => versions?,
    };
    let authenticated =
        matrix_sdk::ruma::api::client::authenticated_media::get_content::v1::Request::PATH_BUILDER
            .is_supported(&versions);
    let mut url = client.homeserver();
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| anyhow!("Invalid homeserver URL"))?;
        path.pop_if_empty();
        if authenticated {
            path.extend(["_matrix", "client", "v1", "media", "download"]);
        } else {
            path.extend(["_matrix", "media", "v3", "download"]);
        }
        path.push(server.as_str()).push(media_id);
    }
    let started = Instant::now();
    let network = async {
        let mut request = client
            .http_client()
            .get(url)
            .timeout(Duration::from_secs(24 * 60 * 60));
        if authenticated {
            request = request.bearer_auth(
                client
                    .access_token()
                    .ok_or_else(|| anyhow!("Not signed in"))?,
            );
        }
        let mut response = request.send().await?.error_for_status()?;
        let total = response.content_length().or(expected);
        let mut received = 0;
        let mut last_update = Instant::now() - Duration::from_secs(1);
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
            received += chunk.len() as u64;
            if last_update.elapsed() >= Duration::from_millis(100) {
                let elapsed = started.elapsed().as_secs_f64();
                let speed = received as f64 / elapsed.max(0.001);
                let eta = total
                    .filter(|_| elapsed >= 0.5 && speed > 0.)
                    .and_then(|total| {
                        Duration::try_from_secs_f64(total.saturating_sub(received) as f64 / speed)
                            .ok()
                    });
                progress(Progress::Receiving {
                    received,
                    total,
                    speed,
                    eta,
                });
                last_update = Instant::now();
            }
        }
        file.flush()?;
        anyhow::ensure!(
            total.is_none_or(|total| received >= total),
            "Incomplete download"
        );
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! { biased;
        _ = cancel.cancelled() => bail!("Cancelled"),
        result = network => result?,
    }
    progress(Progress::Saving);
    let cancel_save = cancel.clone();
    tokio::task::spawn_blocking(move || {
        anyhow::ensure!(!cancel_save.is_cancelled(), "Cancelled");
        let file = if let MediaSource::Encrypted(info) = source {
            let mut encrypted = CancellableReader {
                inner: file.reopen()?,
                cancel: cancel_save.clone(),
            };
            let mut decrypted = tempfile::Builder::new()
                .prefix(".huddle-")
                .suffix(".part")
                .tempfile_in(&directory)?;
            let mut reader =
                matrix_sdk_crypto::AttachmentDecryptor::new(&mut encrypted, (*info).into())?;
            std::io::copy(&mut reader, &mut decrypted)?;
            decrypted
        } else {
            file
        };
        anyhow::ensure!(!cancel_save.is_cancelled(), "Cancelled");
        publish(file, &directory, &name, &cancel_save)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_are_portable_and_cannot_escape_the_download_folder() {
        assert_eq!(filename("../../setup.exe"), "setup.exe");
        assert_eq!(filename(r"C:\temp\CON.exe"), "_CON.exe");
        assert_eq!(filename(".."), "download");
        assert_eq!(filename("a:b?.exe"), "a_b_.exe");
        assert!(filename(&"ą".repeat(300)).len() <= 200);
    }
    #[test]
    fn download_folder_settings_round_trip_and_reset_without_losing_unicode() {
        let folder = tempfile::tempdir().unwrap();
        let config = folder.path().join("downloads.json");
        let target = folder.path().join("Pobrane zdjęcia").join("huddle-client");
        save_settings(
            &config,
            &Settings {
                directory: Some(target.clone()),
            },
        )
        .unwrap();
        let restored: Settings = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(restored.directory, Some(target));
        save_settings(&config, &Settings::default()).unwrap();
        let restored: Settings = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(restored.directory, None);
        assert_eq!(default_directory().file_name().unwrap(), "huddle-client");
        assert!(set_directory(Some(PathBuf::from("relative/path"))).is_err());
    }

    #[test]
    fn completed_downloads_do_not_overwrite_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("setup.exe"), b"original").unwrap();
        let mut file = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        file.write_all(b"new file").unwrap();
        let saved = publish(file, directory.path(), "setup.exe", &Cancel::default()).unwrap();
        assert_eq!(saved.file_name().unwrap(), "setup (1).exe");
        assert_eq!(std::fs::read(saved).unwrap(), b"new file");
        assert_eq!(
            std::fs::read(directory.path().join("setup.exe")).unwrap(),
            b"original"
        );
    }
}
