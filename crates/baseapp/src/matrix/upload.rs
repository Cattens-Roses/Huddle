//! Bounded file reads shared by picking, pasting, and dropping attachments.
use std::{io::Read as _, path::Path};

pub(super) fn read(path: &Path) -> Result<(String, Vec<u8>), &'static str> {
    let metadata = path
        .metadata()
        .map_err(|_| "That file couldn't be opened.")?;
    if !metadata.is_file() {
        return Err("Choose a file, not a folder.");
    }
    if metadata.len() > super::UPLOAD_LIMIT as u64 {
        return Err("That file is too large. Choose one under 20 MB.");
    }
    let file = std::fs::File::open(path).map_err(|_| "That file couldn't be opened.")?;
    let mut bytes = Vec::new();
    file.take(super::UPLOAD_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "That file couldn't be opened.")?;
    if bytes.len() > super::UPLOAD_LIMIT {
        return Err("That file is too large. Choose one under 20 MB.");
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    Ok((name, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upload_reads_supported_media_without_changing_bytes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/clip.mp4");
        let (name, bytes) = read(&path).unwrap();
        assert_eq!(name, "clip.mp4");
        assert_eq!(bytes, include_bytes!("../../tests/media/clip.mp4"));
    }
    #[test]
    fn upload_rejects_directories_and_missing_files() {
        assert!(read(&std::env::temp_dir()).is_err());
        assert!(
            read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/missing.file")).is_err()
        );
    }
    #[test]
    fn upload_rejects_oversized_files_before_reading() {
        let path = std::env::temp_dir().join(format!(
            "huddle-upload-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(super::super::UPLOAD_LIMIT as u64 + 1).unwrap();
        drop(file);
        let result = read(&path);
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            result.unwrap_err(),
            "That file is too large. Choose one under 20 MB."
        );
    }
}
