//! Native media decoding and playback. No browser or external player is used.
mod audio;
mod opus;

use std::io::Cursor;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use rodio::{OutputStream, Sink, Source};
use rumpeg::codec::DecoderContext;
use rumpeg::format::{FormatContext, MediaIo};
use rumpeg::{AudioFrame, Frame, MediaType, PixelFormat, SampleFormat};

#[derive(Clone, Default)]
pub(super) struct Metadata {
    pub width: u32,
    pub height: u32,
    pub duration: Option<Duration>,
    pub thumbnail: Option<Thumbnail>,
}

#[derive(Clone)]
pub(super) struct Thumbnail {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn pixels(
    frame: &rumpeg::VideoFrame,
    longest: u32,
    format: PixelFormat,
) -> Result<rumpeg::VideoFrame> {
    if frame.width == 0 || frame.height == 0 {
        bail!("Empty video frame");
    }
    let scale = (longest as f64 / frame.width.max(frame.height) as f64).min(1.);
    Ok(rumpeg::scale::transform(
        frame,
        format,
        (frame.width as f64 * scale).max(1.) as u32,
        (frame.height as f64 * scale).max(1.) as u32,
        rumpeg::scale::FilterMode::Bilinear,
    )?)
}

fn display_pixels(frame: &rumpeg::VideoFrame) -> Result<image::RgbaImage> {
    let rgb = pixels(frame, 1280, PixelFormat::Rgb24)?;
    let source = rgb
        .plane(0)
        .ok_or_else(|| anyhow!("Missing video pixels"))?;
    let mut image = image::RgbaImage::new(rgb.width, rgb.height);
    for (source, target) in source.chunks_exact(3).zip(image.chunks_exact_mut(4)) {
        target.copy_from_slice(&[source[2], source[1], source[0], 255]);
    }
    Ok(image)
}

fn poster(frame: &rumpeg::VideoFrame) -> Result<Thumbnail> {
    let rgb = pixels(frame, 640, PixelFormat::Rgb24)?;
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85).encode(
        rgb.plane(0)
            .ok_or_else(|| anyhow!("Missing video pixels"))?,
        rgb.width,
        rgb.height,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(Thumbnail {
        bytes,
        width: rgb.width,
        height: rgb.height,
    })
}

/// Decode just the first displayable video frame, without opening audio output.
pub(super) fn thumbnail(bytes: Vec<u8>) -> Result<Thumbnail> {
    std::panic::catch_unwind(|| {
        let mut input = container(bytes)?;
        let stream = input
            .best_stream(MediaType::Video)
            .ok_or_else(|| anyhow!("No video track"))?
            .clone();
        let mut decoder = DecoderContext::open(&stream.codec_params)?;
        loop {
            let packet = match input.read_packet() {
                Ok(packet) => packet,
                Err(rumpeg::Error::Eof) => break,
                Err(error) => return Err(error.into()),
            };
            if packet.stream_index == stream.index {
                for frame in decoder.decode(&packet)? {
                    if let Frame::Video(frame) = frame {
                        return poster(&frame);
                    }
                }
            }
        }
        decoder.send_eof()?;
        while let Ok(frame) = decoder.receive_frame() {
            if let Frame::Video(frame) = frame {
                return poster(&frame);
            }
        }
        bail!("No decodable video frames")
    })
    .unwrap_or_else(|_| Err(anyhow!("Malformed video data")))
}

pub(super) fn mime(name: &str) -> Option<&'static str> {
    Some(
        match name.rsplit('.').next()?.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => "video/mp4",
            "mov" => "video/quicktime",
            "mkv" => "video/x-matroska",
            "webm" => "video/webm",
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            "aac" => "audio/aac",
            "wav" => "audio/wav",
            "flac" => "audio/flac",
            "ogg" | "oga" | "opus" => "audio/ogg",
            _ => return None,
        },
    )
}

fn audio_source(bytes: Vec<u8>) -> Result<Box<dyn Source<Item = f32> + Send>> {
    if opus::is_opus(&bytes) {
        Ok(Box::new(opus::OggOpus::new(bytes)?))
    } else if bytes.get(4..8) == Some(b"ftyp") {
        Ok(Box::new(audio::Mp4Audio::new(bytes)?))
    } else {
        Ok(Box::new(
            rodio::Decoder::new(Cursor::new(bytes))?.convert_samples::<f32>(),
        ))
    }
}

fn container(bytes: Vec<u8>) -> Result<FormatContext> {
    rumpeg::init();
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        // Rumpeg omits unknown Matroska tracks. Reject these explicitly rather
        // than letting a video with unsupported audio play silently.
        use matroska_demuxer::{MatroskaFile, TrackType};
        let file = MatroskaFile::open(Cursor::new(bytes.clone()))?;
        for track in file.tracks().iter().filter(|track| track.flag_enabled()) {
            let supported = match track.track_type() {
                TrackType::Video => matches!(
                    track.codec_id(),
                    "V_MPEG4/ISO/AVC" | "V_MPEGH/ISO/HEVC" | "V_VP9"
                ),
                TrackType::Audio => {
                    matches!(
                        track.codec_id(),
                        "A_FLAC"
                            | "A_OPUS"
                            | "A_AAC"
                            | "A_AAC/MPEG2/Main"
                            | "A_AAC/MPEG2/LC"
                            | "A_AAC/MPEG4/Main"
                            | "A_AAC/MPEG4/LC"
                            | "A_MPEG/L3"
                    ) || (track.codec_id() == "A_PCM/INT/LIT"
                        && track
                            .audio()
                            .and_then(|audio| audio.bit_depth())
                            .is_some_and(|bits| bits.get() == 16))
                }
                _ => true,
            };
            if !supported {
                bail!("Unsupported media track: {}", track.codec_id());
            }
        }
    }
    Ok(FormatContext::open_io(MediaIo::from_bytes(bytes))?)
}

// The video demuxer can omit standard MP4 AAC tracks with nested ES descriptors.
// Detect sound tracks independently and let Symphonia handle MP4 audio.
fn mp4_has_audio(bytes: &[u8]) -> bool {
    fn scan(mut bytes: &[u8], depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        while bytes.len() >= 8 {
            let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            let kind = &bytes[4..8];
            let (size, header) = if size == 1 && bytes.len() >= 16 {
                let Ok(size) =
                    usize::try_from(u64::from_be_bytes(bytes[8..16].try_into().unwrap()))
                else {
                    return false;
                };
                (size, 16)
            } else if size == 0 {
                (bytes.len(), 8)
            } else {
                (size, 8)
            };
            if size < header || size > bytes.len() {
                return false;
            }
            let payload = &bytes[header..size];
            if kind == b"hdlr" && payload.get(8..12) == Some(b"soun") {
                return true;
            }
            if matches!(kind, b"moov" | b"trak" | b"mdia") && scan(payload, depth + 1) {
                return true;
            }
            bytes = &bytes[size..];
        }
        false
    }
    scan(bytes, 0)
}

/// Validate the complete stream before allowing an upload, including its codecs.
pub(super) fn inspect(bytes: &[u8], mime: &str) -> Result<Metadata> {
    std::panic::catch_unwind(|| inspect_inner(bytes, mime))
        .unwrap_or_else(|_| Err(anyhow!("Malformed media data")))
}

fn inspect_inner(bytes: &[u8], mime: &str) -> Result<Metadata> {
    if mime.starts_with("audio/") {
        let matches = match mime {
            "audio/wav" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
            "audio/flac" => bytes.starts_with(b"fLaC"),
            "audio/ogg" => bytes.starts_with(b"OggS"),
            "audio/mp4" => bytes.get(4..8) == Some(b"ftyp"),
            "audio/aac" => {
                bytes.first() == Some(&0xff) && bytes.get(1).is_some_and(|b| b & 0xf6 == 0xf0)
            }
            "audio/mpeg" => {
                bytes.starts_with(b"ID3")
                    || (bytes.first() == Some(&0xff)
                        && bytes.get(1).is_some_and(|b| b & 0xe0 == 0xe0 && b & 6 != 0))
            }
            _ => false,
        };
        if !matches {
            bail!("The file contents don't match its extension.");
        }
        let source = audio_source(bytes.to_vec())?;
        let rate = source.sample_rate() as f64 * source.channels() as f64;
        let duration = source.total_duration();
        // Drive decoding so a header alone is not enough to accept the file.
        let count = if mime == "audio/mp4" {
            audio::Mp4Audio::new(bytes.to_vec())?.validate()?
        } else if opus::is_opus(bytes) {
            opus::OggOpus::new(bytes.to_vec())?.validate()?
        } else {
            source.count()
        };
        if count == 0 {
            bail!("This audio file has no samples.");
        }
        return Ok(Metadata {
            duration: duration.or_else(|| Some(Duration::from_secs_f64(count as f64 / rate))),
            ..Metadata::default()
        });
    }
    let mut input = container(bytes.to_vec())?;
    if input.format_name() == "mp4" && mp4_has_audio(bytes) {
        if audio::Mp4Audio::new(bytes.to_vec())?.validate()? == 0 {
            bail!("This video has no decodable audio samples.");
        }
    }
    let expected = match mime {
        "video/mp4" | "audio/mp4" | "video/quicktime" => "mp4",
        "video/webm" | "video/x-matroska" => "matroska",
        _ => bail!("Unsupported media type"),
    };
    if input.format_name() != expected && !(expected == "matroska" && input.format_name() == "webm")
    {
        bail!("The file contents don't match its extension.");
    }
    let streams: Vec<_> = input
        .streams()
        .iter()
        .filter(|s| matches!(s.media_type, MediaType::Video | MediaType::Audio))
        .cloned()
        .collect();
    if streams.is_empty() {
        bail!("This file has no audio or video tracks.");
    }
    if mime.starts_with("video/") && !streams.iter().any(|s| s.media_type == MediaType::Video) {
        bail!("This file has no video track.");
    }
    let mut metadata = Metadata::default();
    let mut frames = 0;
    let mut decoded_duration = 0f64;
    walk(&mut input, |stream, frame, packet_pts| {
        let pts = frame
            .pts()
            .as_secs(stream.time_base)
            .or_else(|| packet_pts.as_secs(stream.time_base))
            .unwrap_or(0.)
            .max(0.);
        frames += 1;
        match frame {
            Frame::Video(frame) => {
                decoded_duration = decoded_duration.max(pts + 1. / 25.);
                metadata.width = frame.width;
                metadata.height = frame.height;
                if metadata.thumbnail.is_none() {
                    metadata.thumbnail = Some(poster(&frame)?);
                }
                // Verify the same pixel conversion used by the player.
                rumpeg::scale::transform(
                    &frame,
                    PixelFormat::Rgb24,
                    16,
                    16,
                    rumpeg::scale::FilterMode::Bilinear,
                )?;
            }
            Frame::Audio(frame) => {
                decoded_duration = decoded_duration
                    .max(pts + frame.samples as f64 / frame.sample_rate.max(1) as f64);
                samples(&frame)?;
            }
        }
        Ok(true)
    })?;
    if frames == 0 {
        bail!("This media file has no decodable frames.");
    }
    metadata.duration = streams
        .iter()
        .filter_map(|s| {
            let seconds = rumpeg::Timestamp(s.duration?).as_secs(s.time_base)?;
            (seconds.is_finite() && seconds >= 0.).then(|| Duration::from_secs_f64(seconds))
        })
        .max()
        .or_else(|| (decoded_duration > 0.).then(|| Duration::from_secs_f64(decoded_duration)));
    Ok(metadata)
}

fn walk(
    input: &mut FormatContext,
    mut consume: impl FnMut(&rumpeg::format::Stream, Frame, rumpeg::Timestamp) -> Result<bool>,
) -> Result<()> {
    let streams: Vec<_> = [MediaType::Video, MediaType::Audio]
        .into_iter()
        .filter(|kind| *kind != MediaType::Audio || input.format_name() != "mp4")
        .filter_map(|kind| input.best_stream(kind).cloned())
        .collect();
    let mp4 = input.format_name() == "mp4";
    let mut pending_video: Vec<(
        rumpeg::format::Stream,
        rumpeg::VideoFrame,
        rumpeg::Timestamp,
    )> = Vec::new();
    let mut decoders = streams
        .iter()
        .map(|stream| Ok((stream, DecoderContext::open(&stream.codec_params)?)))
        .collect::<Result<Vec<_>>>()?;
    // MP4 may store whole tracks in separate file regions. Interleave compressed
    // packets by decoding time so sound does not start after the video ends.
    let mut packets = Vec::new();
    loop {
        let packet = match input.read_packet() {
            Ok(packet) => packet,
            Err(rumpeg::Error::Eof) => break,
            Err(error) => return Err(error.into()),
        };
        if let Some(stream) = streams.iter().find(|s| s.index == packet.stream_index) {
            let time = packet
                .dts
                .as_secs(stream.time_base)
                .or_else(|| packet.pts.as_secs(stream.time_base))
                .unwrap_or(0.);
            packets.push((time, packet));
        }
    }
    // MP4 has real decode timestamps. Matroska's demuxer exposes presentation
    // timestamps as DTS, so sorting those would feed B-frames to the decoder
    // before their reference pictures. Preserve Matroska's encoded packet order.
    if mp4 {
        packets.sort_by(|(a, _), (b, _)| a.total_cmp(b));
    }
    for (_, packet) in packets {
        if let Some((stream, decoder)) = decoders
            .iter_mut()
            .find(|(s, _)| s.index == packet.stream_index)
        {
            for frame in decoder.decode(&packet)? {
                match frame {
                    Frame::Video(frame) => {
                        pending_video.push(((*stream).clone(), frame, packet.pts))
                    }
                    frame => {
                        if !consume(stream, frame, packet.pts)? {
                            return Ok(());
                        }
                    }
                }
            }
            pending_video
                .sort_by_key(|(_, frame, pts)| if frame.pts.is_none() { *pts } else { frame.pts });
            let watermark = if mp4 {
                packet.dts.as_secs(stream.time_base)
            } else {
                None
            };
            while pending_video.first().is_some_and(|(s, frame, pts)| {
                pending_video.len() > 16
                    || watermark.is_some_and(|time| {
                        frame
                            .pts
                            .as_secs(s.time_base)
                            .or_else(|| pts.as_secs(s.time_base))
                            .is_some_and(|pts| pts <= time)
                    })
            }) {
                let (stream, frame, pts) = pending_video.remove(0);
                if !consume(&stream, Frame::Video(frame), pts)? {
                    return Ok(());
                }
            }
        }
    }
    for (stream, decoder) in &mut decoders {
        decoder.send_eof()?;
        loop {
            match decoder.receive_frame() {
                Ok(frame) => match frame {
                    Frame::Video(frame) => {
                        pending_video.push(((*stream).clone(), frame, rumpeg::Timestamp::NONE))
                    }
                    frame => {
                        if !consume(stream, frame, rumpeg::Timestamp::NONE)? {
                            return Ok(());
                        }
                    }
                },
                Err(rumpeg::Error::Eof | rumpeg::Error::NeedMoreData | rumpeg::Error::TryAgain) => {
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    pending_video.sort_by_key(|(_, frame, pts)| if frame.pts.is_none() { *pts } else { frame.pts });
    for (stream, frame, pts) in pending_video {
        if !consume(&stream, Frame::Video(frame), pts)? {
            break;
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct State {
    pub frame: Option<Arc<gpui::RenderImage>>,
    pub error: Option<String>,
    pub finished: bool,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub seeking: bool,
}

struct Controls {
    volume: AtomicU32,
    muted: AtomicBool,
    seek: Mutex<Option<Duration>>,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            volume: AtomicU32::new(100),
            muted: AtomicBool::new(false),
            seek: Mutex::new(None),
        }
    }
}

impl Controls {
    fn interrupted(&self, stop: &AtomicBool) -> bool {
        stop.load(Ordering::Relaxed)
            || self
                .seek
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }
    fn gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.
        } else {
            self.volume.load(Ordering::Relaxed) as f32 / 100.
        }
    }
}

pub(super) fn time_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

pub(super) struct Player {
    pub name: String,
    pub state: Arc<Mutex<State>>,
    pub paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    controls: Arc<Controls>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Player {
    pub fn new(name: String, bytes: Vec<u8>, mime: String) -> Self {
        let player = Self {
            name,
            state: Arc::default(),
            paused: Arc::default(),
            stop: Arc::default(),
            controls: Arc::default(),
        };
        let state = player.state.clone();
        let paused = player.paused.clone();
        let stop = player.stop.clone();
        let controls = player.controls.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_player(bytes, &mime, &state, &paused, &stop, &controls)
            }))
            .unwrap_or_else(|_| Err(anyhow!("Malformed media data")));
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(error) = result {
                log::warn!("Media playback failed: {error:#}");
                state.error = Some("This file couldn't be played. Its codec may be unsupported, or audio output is unavailable.".into());
            }
            state.finished = true;
        });
        player
    }

    pub fn volume(&self) -> u32 {
        self.controls.volume.load(Ordering::Relaxed)
    }
    pub fn is_muted(&self) -> bool {
        self.controls.muted.load(Ordering::Relaxed)
    }
    pub fn change_volume(&self, delta: i32) {
        self.controls
            .volume
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some((current as i32 + delta).clamp(0, 100) as u32)
            })
            .ok();
        self.controls.muted.store(false, Ordering::Relaxed);
    }
    pub fn mute(&self) {
        self.controls.muted.fetch_xor(true, Ordering::Relaxed);
    }
    pub fn skip(&self, seconds: i64) {
        let mut target = self.controls.seek.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let position = target.unwrap_or(state.position);
        let position = if seconds < 0 {
            position.saturating_sub(Duration::from_secs(seconds.unsigned_abs()))
        } else {
            position.saturating_add(Duration::from_secs(seconds as u64))
        };
        Self::queue_seek(&mut target, &mut state, position);
    }
    pub fn seek_to(&self, position: Duration) {
        let mut target = self.controls.seek.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::queue_seek(&mut target, &mut state, position);
    }
    fn queue_seek(target: &mut Option<Duration>, state: &mut State, position: Duration) {
        let position = state
            .duration
            .map_or(position, |duration| position.min(duration));
        *target = Some(position);
        state.position = position;
        state.finished = false;
        state.seeking = true;
    }
    pub fn restart(&self) {
        self.seek_to(Duration::ZERO);
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn toggle(&self) {
        self.paused.fetch_xor(true, Ordering::Relaxed);
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

fn run_player(
    bytes: Vec<u8>,
    mime: &str,
    state: &Mutex<State>,
    paused: &AtomicBool,
    stop: &AtomicBool,
    controls: &Controls,
) -> Result<()> {
    let mut start = Duration::ZERO;
    loop {
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.finished = false;
            state.error = None;
        }
        play_segment(bytes.clone(), mime, state, paused, stop, controls, start)?;
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(target) = controls
            .seek
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            start = target;
            continue;
        }
        state.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if let Some(target) = controls
                .seek
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                start = target;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
fn play(
    bytes: Vec<u8>,
    mime: &str,
    state: &Mutex<State>,
    paused: &AtomicBool,
    stop: &AtomicBool,
) -> Result<()> {
    play_segment(
        bytes,
        mime,
        state,
        paused,
        stop,
        &Controls::default(),
        Duration::ZERO,
    )
}

fn wait_controlled(
    paused: &AtomicBool,
    stop: &AtomicBool,
    sink: Option<&Sink>,
    clock: &mut Instant,
    target: f64,
    controls: &Controls,
    state: &Mutex<State>,
    start: Duration,
) -> bool {
    loop {
        if controls.interrupted(stop) {
            return false;
        }
        if let Some(sink) = sink {
            sink.set_volume(controls.gain());
        }
        if paused.load(Ordering::Relaxed) {
            if let Some(sink) = sink {
                sink.pause();
            }
            let before = Instant::now();
            std::thread::sleep(Duration::from_millis(5));
            *clock += before.elapsed();
            continue;
        }
        if let Some(sink) = sink {
            sink.play();
        }
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            let position = start.saturating_add(clock.elapsed());
            state.position = state
                .duration
                .map_or(position, |duration| position.min(duration));
        }
        if clock.elapsed().as_secs_f64() >= target {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
fn wait(
    paused: &AtomicBool,
    stop: &AtomicBool,
    sink: Option<&Sink>,
    clock: &mut Instant,
    target: f64,
) -> bool {
    wait_controlled(
        paused,
        stop,
        sink,
        clock,
        target,
        &Controls::default(),
        &Mutex::new(State::default()),
        Duration::ZERO,
    )
}

fn play_segment(
    bytes: Vec<u8>,
    mime: &str,
    state: &Mutex<State>,
    paused: &AtomicBool,
    stop: &AtomicBool,
    controls: &Controls,
    start: Duration,
) -> Result<()> {
    if mime.starts_with("audio/") {
        let source = audio_source(bytes)?;
        let duration = source.total_duration();
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.duration = duration.or(state.duration);
            state.position = start;
            state.seeking = false;
        }
        let (_output, handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&handle)?;
        sink.pause();
        sink.set_volume(controls.gain());
        sink.append(source.skip_duration(start));
        let mut clock = Instant::now();
        while !sink.empty() {
            if !wait_controlled(
                paused,
                stop,
                Some(&sink),
                &mut clock,
                0.,
                controls,
                state,
                start,
            ) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        return Ok(());
    }
    let separate_audio = if mp4_has_audio(&bytes) {
        Some(audio::Mp4Audio::new(bytes.clone())?)
    } else {
        None
    };
    let mut input = container(bytes)?;
    let streams: Vec<_> = [MediaType::Video, MediaType::Audio]
        .into_iter()
        .filter_map(|kind| input.best_stream(kind).cloned())
        .collect();
    if !streams
        .iter()
        .any(|stream| stream.media_type == MediaType::Video)
    {
        bail!("No video track.");
    }
    let duration = streams
        .iter()
        .filter_map(|s| {
            s.duration
                .and_then(|d| rumpeg::Timestamp(d).as_secs(s.time_base))
        })
        .filter(|d| d.is_finite() && *d > 0.)
        .map(Duration::from_secs_f64)
        .max();
    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.duration = duration.or(state.duration);
        state.position = start;
    }
    let output =
        if separate_audio.is_some() || streams.iter().any(|s| s.media_type == MediaType::Audio) {
            Some(OutputStream::try_default()?)
        } else {
            None
        };
    let sink = output
        .as_ref()
        .map(|(_, handle)| Sink::try_new(handle))
        .transpose()?;
    if let Some(sink) = &sink {
        sink.pause();
        sink.set_volume(controls.gain());
    }
    if let (Some(source), Some(sink)) = (separate_audio, sink.as_ref()) {
        sink.append(source.skip_duration(start));
    }
    let mut clock = Instant::now();
    let mut first_pts = None;
    let mut fallback = 0.;
    let mut started = false;
    let mut last_target = 0.;
    let mut last_skipped = None;
    let mut observed_end = 0f64;
    let mut previous_video = None;
    walk(&mut input, |stream, frame, packet_pts| {
        if controls.interrupted(stop) {
            return Ok(false);
        }
        match frame {
            Frame::Audio(frame) => {
                let pts = frame
                    .pts
                    .as_secs(stream.time_base)
                    .or_else(|| packet_pts.as_secs(stream.time_base))
                    .unwrap_or(0.);
                observed_end = observed_end
                    .max(pts.max(0.) + frame.samples as f64 / frame.sample_rate.max(1) as f64);
                let trim =
                    ((start.as_secs_f64() - pts).max(0.) * frame.sample_rate as f64) as usize;
                if trim >= frame.samples as usize {
                    return Ok(true);
                }
                if let Some(sink) = &sink {
                    let channels = frame.layout.channels as usize;
                    let data = samples(&frame)?;
                    sink.append(rodio::buffer::SamplesBuffer::new(
                        channels as u16,
                        frame.sample_rate,
                        data[trim * channels..].to_vec(),
                    ));
                    while started && sink.len() > 100 {
                        if !wait_controlled(
                            paused,
                            stop,
                            Some(sink),
                            &mut clock,
                            0.,
                            controls,
                            state,
                            start,
                        ) {
                            return Ok(false);
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            Frame::Video(frame) => {
                let pts = frame
                    .pts
                    .as_secs(stream.time_base)
                    .or_else(|| packet_pts.as_secs(stream.time_base))
                    .unwrap_or(fallback);
                let origin = *first_pts.get_or_insert(pts);
                let position = (pts - origin).max(0.);
                let gap = previous_video
                    .map(|previous: f64| (position - previous).max(0.))
                    .unwrap_or(1. / 30.);
                observed_end = observed_end.max(position + gap);
                previous_video = Some(position);
                fallback = pts + 1. / 30.;
                if position + 0.001 < start.as_secs_f64() {
                    last_skipped = Some(frame);
                    return Ok(true);
                }
                let target = (position - start.as_secs_f64()).max(0.);
                if !started {
                    clock = Instant::now();
                    started = true;
                    let image = display_pixels(&frame)?;
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    state.frame = Some(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
                        image,
                    )])));
                    state.seeking = false;
                    state.position = Duration::from_secs_f64(position);
                }
                // Reordered frames must never move the presentation clock backwards.
                if target < last_target {
                    return Ok(true);
                }
                last_target = target;
                if target > 0. && clock.elapsed().as_secs_f64() > target + 0.1 {
                    return Ok(true);
                }
                let image = display_pixels(&frame)?;
                if !wait_controlled(
                    paused,
                    stop,
                    sink.as_ref(),
                    &mut clock,
                    target,
                    controls,
                    state,
                    start,
                ) {
                    return Ok(false);
                }
                state.lock().unwrap_or_else(|e| e.into_inner()).frame = Some(Arc::new(
                    gpui::RenderImage::new(vec![image::Frame::new(image)]),
                ));
            }
        }
        Ok(true)
    })?;
    if !controls.interrupted(stop)
        && !started
        && let Some(frame) = last_skipped
    {
        let image = display_pixels(&frame)?;
        state.lock().unwrap_or_else(|e| e.into_inner()).frame = Some(Arc::new(
            gpui::RenderImage::new(vec![image::Frame::new(image)]),
        ));
    }
    if let Some(sink) = &sink {
        while !sink.empty() && started {
            if !wait_controlled(
                paused,
                stop,
                Some(sink),
                &mut clock,
                0.,
                controls,
                state,
                start,
            ) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if !controls.interrupted(stop) {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.seeking = false;
        if state.duration.is_none() && observed_end > 0. {
            state.duration = Some(Duration::from_secs_f64(observed_end));
        }
        if let Some(duration) = state.duration {
            state.position = duration;
        }
    }
    Ok(())
}

fn samples(frame: &AudioFrame) -> Result<Vec<f32>> {
    if frame.layout.channels == 0 || frame.sample_rate == 0 {
        bail!("Invalid audio parameters");
    }
    let size = frame.format.bytes_per_sample();
    let mut result = Vec::with_capacity(frame.samples as usize * frame.layout.channels as usize);
    for sample in 0..frame.samples as usize {
        for channel in 0..frame.layout.channels as usize {
            let (plane, offset) = if frame.format.is_planar() {
                (channel, sample * size)
            } else {
                (
                    0,
                    (sample * frame.layout.channels as usize + channel) * size,
                )
            };
            let b = frame
                .planes
                .get(plane)
                .and_then(|p| p.as_slice().get(offset..offset + size))
                .ok_or_else(|| anyhow!("Truncated audio frame"))?;
            let value = match frame.format {
                SampleFormat::U8 | SampleFormat::U8P => (b[0] as f32 - 128.) / 128.,
                SampleFormat::S16 | SampleFormat::S16P => {
                    i16::from_le_bytes(b.try_into()?) as f32 / 32768.
                }
                SampleFormat::S24 => {
                    ((i32::from_le_bytes([0, b[0], b[1], b[2]])) >> 8) as f32 / 8388608.
                }
                SampleFormat::S32 | SampleFormat::S32P => {
                    i32::from_le_bytes(b.try_into()?) as f32 / 2147483648.
                }
                SampleFormat::F32 | SampleFormat::F32P => f32::from_le_bytes(b.try_into()?),
                SampleFormat::F64 | SampleFormat::F64P => f64::from_le_bytes(b.try_into()?) as f32,
                _ => bail!("Unsupported audio sample format"),
            };
            result.push(if value.is_finite() {
                value.clamp(-1., 1.)
            } else {
                0.
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rumpeg::{
        Buffer, ChannelLayout, CodecId, CodecParams, Rational, Timestamp, VideoFrame, VideoParams,
    };

    const MP4: &[u8] = include_bytes!("../../tests/media/clip.mp4");

    #[test]
    fn b_frames_are_presented_in_timestamp_order() {
        for bytes in [
            include_bytes!("../../tests/media/reordered.mp4").as_slice(),
            include_bytes!("../../tests/media/reordered.mkv").as_slice(),
        ] {
            let mut input = container(bytes.to_vec()).unwrap();
            let reference = include_bytes!("../../tests/media/reordered.yuv");
            let mut timestamps = Vec::new();
            let mut errors = Vec::new();
            walk(&mut input, |stream, frame, packet_pts| {
                if let Frame::Video(frame) = frame {
                    timestamps.push(
                        frame
                            .pts
                            .as_secs(stream.time_base)
                            .or_else(|| packet_pts.as_secs(stream.time_base))
                            .unwrap(),
                    );
                    let index = errors.len();
                    let expected = &reference[index * 9216..index * 9216 + 6144];
                    let actual = frame.plane(0).unwrap();
                    errors.push(
                        actual
                            .iter()
                            .zip(expected)
                            .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
                            .sum::<f64>()
                            / 6144.,
                    );
                }
                Ok(true)
            })
            .unwrap();
            assert_eq!(timestamps.len(), 24);
            assert!(
                timestamps.windows(2).all(|pair| pair[0] < pair[1]),
                "Frames jump backwards: {timestamps:?}"
            );
            assert!(
                errors.iter().all(|mse| *mse < 5.),
                "Corrupted decoded frames: {errors:?}"
            );
        }
    }

    fn fixtures() -> [(&'static str, &'static [u8]); 12] {
        [
            ("clip.mp4", MP4),
            (
                "clip.m4v",
                include_bytes!("../../tests/media/clip.m4v").as_slice(),
            ),
            (
                "clip.mov",
                include_bytes!("../../tests/media/clip.mov").as_slice(),
            ),
            (
                "clip.mkv",
                include_bytes!("../../tests/media/clip.mkv").as_slice(),
            ),
            (
                "clip.webm",
                include_bytes!("../../tests/media/clip.webm").as_slice(),
            ),
            (
                "tone.mp3",
                include_bytes!("../../tests/media/tone.mp3").as_slice(),
            ),
            (
                "tone.m4a",
                include_bytes!("../../tests/media/tone.m4a").as_slice(),
            ),
            (
                "tone.aac",
                include_bytes!("../../tests/media/tone.aac").as_slice(),
            ),
            (
                "tone.wav",
                include_bytes!("../../tests/media/tone.wav").as_slice(),
            ),
            (
                "tone.flac",
                include_bytes!("../../tests/media/tone.flac").as_slice(),
            ),
            (
                "tone.ogg",
                include_bytes!("../../tests/media/tone.ogg").as_slice(),
            ),
            (
                "tone.OGA",
                include_bytes!("../../tests/media/tone.ogg").as_slice(),
            ),
        ]
    }

    #[test]
    fn independently_encoded_formats_validate_and_preview() {
        for (name, bytes) in fixtures() {
            let mime = mime(name).unwrap();
            let metadata = inspect(bytes, mime).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(
                metadata.duration.is_some_and(|d| d > Duration::ZERO),
                "{name}: duration"
            );
            if mime.starts_with("video/") {
                assert_eq!((metadata.width, metadata.height), (64, 48), "{name}");
                let image = metadata.thumbnail.unwrap();
                assert_eq!(image::load_from_memory(&image.bytes).unwrap().width(), 64);
                assert_eq!(thumbnail(bytes.to_vec()).unwrap().bytes, image.bytes);
                let mut input = container(bytes.to_vec()).unwrap();
                let mut videos = 0;
                let mut audio_samples = if mp4_has_audio(bytes) {
                    audio::Mp4Audio::new(bytes.to_vec())
                        .unwrap()
                        .validate()
                        .unwrap()
                } else {
                    0
                };
                walk(&mut input, |_, frame, _| {
                    match frame {
                        Frame::Video(frame) => {
                            let converted = display_pixels(&frame)?;
                            assert_eq!(converted.as_raw().len(), 64 * 48 * 4);
                            videos += 1;
                        }
                        Frame::Audio(frame) => audio_samples += samples(&frame)?.len(),
                    }
                    Ok(true)
                })
                .unwrap();
                assert_eq!(videos, 9, "{name}: all frames including EOF");
                assert!(audio_samples > 0, "{name}: audio must not disappear");
            } else {
                assert!(metadata.thumbnail.is_none());
            }
        }
    }

    #[test]
    fn malformed_and_mislabeled_media_are_rejected() {
        assert!(thumbnail(Vec::new()).is_err());
        assert!(thumbnail(b"not a video".to_vec()).is_err());
        assert!(inspect(MP4, "video/webm").is_err());
        assert!(inspect(&MP4[..32], "video/mp4").is_err());
        assert!(inspect(include_bytes!("../../tests/media/tone.wav"), "audio/mpeg").is_err());
    }

    fn wav() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&52u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes
    }

    #[test]
    fn ogg_opus_voice_messages_decode_and_seek() {
        for (bytes, channels) in [
            (include_bytes!("../../tests/media/voice.ogg").as_slice(), 1),
            (
                include_bytes!("../../tests/media/voice-stereo.ogg").as_slice(),
                2,
            ),
        ] {
            let source = audio_source(bytes.to_vec()).unwrap();
            assert_eq!(source.channels(), channels);
            assert_eq!(source.sample_rate(), 48000);
            assert_eq!(source.total_duration(), Some(Duration::from_millis(250)));
            let decoded: Vec<_> = source.collect();
            assert_eq!(decoded.len(), 12000 * channels as usize);
            assert!(decoded.iter().all(|sample| sample.is_finite()));
            assert!(decoded.iter().any(|sample| sample.abs() > 0.01));
            let skipped: Vec<_> = audio_source(bytes.to_vec())
                .unwrap()
                .skip_duration(Duration::from_millis(100))
                .collect();
            assert_eq!(skipped, decoded[4800 * channels as usize..]);
            assert_eq!(
                inspect(bytes, "audio/ogg").unwrap().duration,
                Some(Duration::from_millis(250))
            );
        }
    }

    #[test]
    fn audio_validation_reads_samples_and_duration() {
        let metadata = inspect(&wav(), "audio/wav").unwrap();
        assert_eq!(metadata.duration, Some(Duration::from_millis(1)));
        assert!(inspect(b"not audio", "audio/mpeg").is_err());
        assert!(inspect(&[], "video/mp4").is_err());
        assert!(inspect(&wav(), "video/mp4").is_err());
    }

    #[test]
    fn planar_audio_is_interleaved_and_normalized() {
        let mut frame =
            AudioFrame::alloc_planar(SampleFormat::S16P, 48000, ChannelLayout::new(2), 2);
        frame.planes = vec![
            Buffer::from_vec([32767i16.to_le_bytes(), 0i16.to_le_bytes()].concat()),
            Buffer::from_vec([(-32768i16).to_le_bytes(), 16384i16.to_le_bytes()].concat()),
        ];
        let decoded = samples(&frame).unwrap();
        assert_eq!(decoded, vec![32767. / 32768., -1., 0., 0.5]);
        frame.planes.pop();
        assert!(samples(&frame).is_err());
    }

    #[test]
    fn compressed_audio_can_be_processed_by_the_native_player() {
        for (codec, mime) in [
            (CodecId::Mp3, "audio/mpeg"),
            (CodecId::Flac, "audio/flac"),
            (CodecId::Aac, "audio/aac"),
        ] {
            let params = CodecParams::audio_codec(
                codec,
                rumpeg::AudioParams {
                    sample_rate: 44100,
                    layout: ChannelLayout::STEREO,
                    sample_fmt: SampleFormat::S16,
                    frame_size: 0,
                },
            );
            let mut encoder = rumpeg::codec::EncoderContext::open(&params).unwrap();
            let mut frame = AudioFrame::alloc_interleaved(
                SampleFormat::S16,
                44100,
                ChannelLayout::STEREO,
                8192,
            );
            for (index, sample) in frame.data_mut().chunks_exact_mut(2).enumerate() {
                let value = (((index / 2) as f32 * 0.06).sin() * 1000.0) as i16;
                sample.copy_from_slice(&value.to_le_bytes());
            }
            let mut packets = encoder.encode(&Frame::Audio(frame)).unwrap();
            encoder.send_eof().unwrap();
            while let Ok(packet) = encoder.receive_packet() {
                packets.push(packet);
            }
            let mut bytes = Vec::new();
            for packet in packets {
                if codec == CodecId::Aac {
                    let length = packet.data.len() + 7;
                    // AAC-LC, 44.1 kHz, stereo ADTS header.
                    bytes.extend_from_slice(&[
                        0xff,
                        0xf1,
                        0x50,
                        0x80 | ((length >> 11) as u8),
                        (length >> 3) as u8,
                        ((length & 7) << 5) as u8 | 0x1f,
                        0xfc,
                    ]);
                }
                bytes.extend_from_slice(packet.data.as_slice());
            }
            assert!(inspect(&bytes, mime).is_ok(), "{mime}");
        }
    }

    #[test]
    fn mp4_is_decoded_with_dimensions_and_timestamps() {
        // Create our own tiny H.264 clip; no downloaded or copyrighted fixtures.
        rumpeg::init();
        let params = CodecParams::video_codec(
            CodecId::H264,
            VideoParams {
                width: 32,
                height: 32,
                pix_fmt: PixelFormat::Yuv420p,
                frame_rate: Rational::new(25, 1),
                sample_aspect_ratio: Rational::one(),
            },
        );
        let mut encoder = rumpeg::codec::EncoderContext::open(&params).unwrap();
        let path = std::env::temp_dir().join(format!(
            "huddle-media-test-{}-{}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut output = rumpeg::format::OutputContext::open(&path, Some("mp4")).unwrap();
        output.add_stream(encoder.params().clone()).unwrap();
        output.write_header().unwrap();
        for index in 0..3 {
            let mut frame = VideoFrame::alloc(PixelFormat::Yuv420p, 32, 32);
            frame.plane_mut(0).unwrap().fill(80 + index as u8 * 20);
            frame.plane_mut(1).unwrap().fill(128);
            frame.plane_mut(2).unwrap().fill(128);
            frame.pts = Timestamp(index);
            for packet in encoder.encode(&Frame::Video(frame)).unwrap() {
                output.write_packet(&packet).unwrap();
            }
        }
        encoder.send_eof().unwrap();
        while let Ok(packet) = encoder.receive_packet() {
            output.write_packet(&packet).unwrap();
        }
        output.write_trailer().unwrap();
        drop(output);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let metadata = inspect(&bytes, "video/mp4").unwrap();
        assert_eq!((metadata.width, metadata.height), (32, 32));
        assert!(metadata.duration.unwrap() > Duration::ZERO);
        let mut input = container(bytes).unwrap();
        let mut count = 0;
        walk(&mut input, |_, frame, _| {
            if matches!(frame, Frame::Video(_)) {
                count += 1;
            }
            Ok(true)
        })
        .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn stopped_playback_exits_even_when_paused() {
        assert!(!wait(
            &AtomicBool::new(true),
            &AtomicBool::new(true),
            None,
            &mut Instant::now(),
            100.
        ));
    }

    #[test]
    fn volume_and_mute_update_the_audio_sink() {
        let player = Player::new(
            "silent.mp4".into(),
            include_bytes!("../../tests/media/silent.mp4").to_vec(),
            "video/mp4".into(),
        );
        player.change_volume(-60);
        assert_eq!(player.volume(), 40);
        let (sink, _output) = Sink::new_idle();
        let mut clock = Instant::now();
        let state = Mutex::new(State::default());
        assert!(wait_controlled(
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            Some(&sink),
            &mut clock,
            0.,
            &player.controls,
            &state,
            Duration::ZERO
        ));
        assert!((sink.volume() - 0.4).abs() < 0.001);
        player.mute();
        assert!(player.is_muted());
        assert!(wait_controlled(
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            Some(&sink),
            &mut clock,
            0.,
            &player.controls,
            &state,
            Duration::ZERO
        ));
        assert_eq!(sink.volume(), 0.);
        player.mute();
        assert_eq!(player.volume(), 40);
        assert_eq!(player.controls.gain(), 0.4);
        player.change_volume(-1000);
        assert_eq!(player.volume(), 0);
        player.change_volume(1000);
        assert_eq!(player.volume(), 100);
        player.mute();
        player.change_volume(-10);
        assert!(!player.is_muted());
        assert_eq!(player.volume(), 90);
    }

    #[test]
    fn skip_moves_both_directions_while_paused_and_clamps_to_the_clip() {
        let player = Player::new(
            "seek.mp4".into(),
            include_bytes!("../../tests/media/seek.mp4").to_vec(),
            "video/mp4".into(),
        );
        player.toggle();
        fn settled(player: &Player, expected: f64) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let state = player.state.lock().unwrap();
                assert!(state.error.is_none(), "{:?}", state.error);
                if !state.seeking && (state.position.as_secs_f64() - expected).abs() < 0.2 {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "Expected {expected}s, got {:?}, seeking={}",
                    state.position,
                    state.seeking
                );
                drop(state);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // Wait for the duration and first paused frame to be available.
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.state.lock().unwrap().duration.is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        player.seek_to(Duration::from_millis(3250));
        settled(&player, 3.25);
        assert!(player.is_paused());
        player.seek_to(Duration::ZERO);
        settled(&player, 0.);
        player.skip(10);
        settled(&player, 10.);
        assert!(player.is_paused());
        assert!(player.state.lock().unwrap().frame.is_some());
        player.skip(-10);
        settled(&player, 0.);
        player.skip(-10);
        settled(&player, 0.);
        player.skip(100);
        let duration = player.state.lock().unwrap().duration.unwrap().as_secs_f64();
        settled(&player, duration);
        player.restart();
        assert!(!player.is_paused());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = player.state.lock().unwrap();
            if !state.finished && !state.seeking && state.position < Duration::from_secs(1) {
                break;
            }
            assert!(Instant::now() < deadline, "Replay did not restart");
            drop(state);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn elapsed_time_labels_include_hours() {
        assert_eq!(time_label(Duration::from_secs(65)), "1:05");
        assert_eq!(time_label(Duration::from_secs(3665)), "1:01:05");
    }

    #[test]
    fn native_player_paces_frames_and_resumes_after_pause() {
        let player = Player::new(
            "silent.mp4".into(),
            include_bytes!("../../tests/media/silent.mp4").to_vec(),
            "video/mp4".into(),
        );
        player.toggle();
        std::thread::sleep(Duration::from_millis(60));
        player.state.lock().unwrap().frame.take();
        std::thread::sleep(Duration::from_millis(100));
        {
            let state = player.state.lock().unwrap();
            assert!(!state.finished);
            assert!(
                state.frame.is_none(),
                "No frames should advance while paused"
            );
        }
        let started = Instant::now();
        player.toggle();
        let mut frames = 0;
        loop {
            let mut state = player.state.lock().unwrap();
            if let Some(frame) = state.frame.take() {
                assert_eq!(frame.size(0).width.0, 64);
                frames += 1;
            }
            assert!(state.error.is_none(), "{:?}", state.error);
            if state.finished {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "Player did not finish"
            );
            drop(state);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(frames >= 3, "Expected paced frames, got {frames}");
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "Video played too quickly"
        );
    }

    #[test]
    fn closing_the_player_stops_its_worker() {
        let player = Player::new(
            "silent.mp4".into(),
            include_bytes!("../../tests/media/silent.mp4").to_vec(),
            "video/mp4".into(),
        );
        player.toggle();
        let state = player.state.clone();
        drop(player);
        let started = Instant::now();
        while !state.lock().unwrap().finished {
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state.lock().unwrap().error.is_none());
    }

    #[test]
    fn audio_reaches_the_native_mixer_without_a_hardware_device() {
        for bytes in [
            MP4,
            include_bytes!("../../tests/media/tone.m4a").as_slice(),
            include_bytes!("../../tests/media/tone.mp3").as_slice(),
            include_bytes!("../../tests/media/tone.aac").as_slice(),
            include_bytes!("../../tests/media/tone.wav").as_slice(),
            include_bytes!("../../tests/media/tone.flac").as_slice(),
            include_bytes!("../../tests/media/tone.ogg").as_slice(),
            include_bytes!("../../tests/media/voice.ogg").as_slice(),
            include_bytes!("../../tests/media/voice-stereo.ogg").as_slice(),
        ] {
            let (sink, output) = Sink::new_idle();
            sink.append(audio_source(bytes.to_vec()).unwrap());
            let output: Vec<_> = output.take(200_000).collect();
            assert!(
                output.iter().any(|sample| sample.abs() > 0.001),
                "Mixer produced silence"
            );
            assert!(sink.empty(), "Audio source did not finish");
        }
    }

    #[test]
    fn seeking_mp4_audio_resumes_at_the_requested_samples() {
        for (name, bytes) in [
            (
                "seek-audio.m4a",
                include_bytes!("../../tests/media/seek-audio.m4a").as_slice(),
            ),
            (
                "seek-av.mp4",
                include_bytes!("../../tests/media/seek-av.mp4").as_slice(),
            ),
        ] {
            let source = audio_source(bytes.to_vec()).unwrap();
            let rate = source.sample_rate() as u128;
            let channels = source.channels() as u128;
            let decoded: Vec<_> = source.collect();
            for target in [
                Duration::ZERO,
                Duration::from_millis(37),
                Duration::from_secs(10),
                Duration::from_millis(11_500),
                Duration::from_secs(20),
            ] {
                let offset = ((target.as_nanos() * rate / 1_000_000_000) * channels) as usize;
                let expected: Vec<_> = decoded.iter().skip(offset).take(4096).copied().collect();
                let actual: Vec<_> = audio_source(bytes.to_vec())
                    .unwrap()
                    .skip_duration(target)
                    .take(4096)
                    .collect();
                assert_eq!(
                    actual.len(),
                    expected.len(),
                    "{name} at {target:?}: wrong remaining sample count"
                );
                assert!(
                    actual.iter().zip(&expected).all(|(a, b)| a == b),
                    "{name} at {target:?}: playback resumed at the wrong audio position"
                );
                if target == Duration::from_secs(10) {
                    let (sink, output) = Sink::new_idle();
                    sink.append(audio_source(bytes.to_vec()).unwrap().skip_duration(target));
                    let mixed: Vec<_> = output.take(expected.len()).collect();
                    assert!(
                        mixed.iter().zip(&expected).all(|(a, b)| a == b),
                        "{name}: mixer restarted audio after the seek"
                    );
                }
            }
        }
    }

    #[test]
    fn decoded_pixels_use_gpui_channel_order() {
        let mut frame = VideoFrame::alloc(PixelFormat::Rgb24, 1, 1);
        frame.plane_mut(0).unwrap().copy_from_slice(&[240, 80, 20]);
        assert_eq!(
            display_pixels(&frame).unwrap().as_raw(),
            &[20, 80, 240, 255]
        );
    }

    #[test]
    #[ignore = "Requires a system audio device"]
    fn system_audio_and_video_playback_smoke() {
        for (name, bytes) in fixtures() {
            let state = Mutex::new(State::default());
            let mime = mime(name).unwrap();
            play(
                bytes.to_vec(),
                mime,
                &state,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
            )
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
            if mime.starts_with("video/") {
                assert!(state.lock().unwrap().frame.is_some(), "{name}");
            }
        }
    }
}
