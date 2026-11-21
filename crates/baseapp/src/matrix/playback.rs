//! Native media decoding and playback. No browser or external player is used.
use std::io::Cursor;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
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
            "ogg" | "oga" => "audio/ogg",
            _ => return None,
        },
    )
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
        let source = rodio::Decoder::new(Cursor::new(bytes.to_vec()))?;
        let rate = source.sample_rate() as f64 * source.channels() as f64;
        let duration = source.total_duration();
        // Drive decoding so a header alone is not enough to accept the file.
        let count = source.count();
        if count == 0 {
            bail!("This audio file has no samples.");
        }
        return Ok(Metadata {
            duration: duration.or_else(|| Some(Duration::from_secs_f64(count as f64 / rate))),
            ..Metadata::default()
        });
    }
    let mut input = container(bytes.to_vec())?;
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
    walk(&mut input, |_, frame, _| {
        frames += 1;
        match frame {
            Frame::Video(frame) => {
                metadata.width = frame.width;
                metadata.height = frame.height;
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
        .max();
    Ok(metadata)
}

fn walk(
    input: &mut FormatContext,
    mut consume: impl FnMut(&rumpeg::format::Stream, Frame, rumpeg::Timestamp) -> Result<bool>,
) -> Result<()> {
    let streams: Vec<_> = [MediaType::Video, MediaType::Audio]
        .into_iter()
        .filter_map(|kind| input.best_stream(kind).cloned())
        .collect();
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
    packets.sort_by(|(a, _), (b, _)| a.total_cmp(b));
    for (_, packet) in packets {
        if let Some((stream, decoder)) = decoders
            .iter_mut()
            .find(|(s, _)| s.index == packet.stream_index)
        {
            for frame in decoder.decode(&packet)? {
                if !consume(stream, frame, packet.pts)? {
                    return Ok(());
                }
            }
        }
    }
    for (stream, decoder) in &mut decoders {
        decoder.send_eof()?;
        loop {
            match decoder.receive_frame() {
                Ok(frame) => {
                    if !consume(stream, frame, rumpeg::Timestamp::NONE)? {
                        return Ok(());
                    }
                }
                Err(rumpeg::Error::Eof | rumpeg::Error::NeedMoreData | rumpeg::Error::TryAgain) => {
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct State {
    pub frame: Option<Vec<u8>>,
    pub error: Option<String>,
    pub finished: bool,
}

pub(super) struct Player {
    pub name: String,
    pub state: Arc<Mutex<State>>,
    pub paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
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
        };
        let state = player.state.clone();
        let paused = player.paused.clone();
        let stop = player.stop.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                play(bytes, &mime, &state, &paused, &stop)
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

    pub fn toggle(&self) {
        self.paused.fetch_xor(true, Ordering::Relaxed);
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

fn wait(
    paused: &AtomicBool,
    stop: &AtomicBool,
    sink: Option<&Sink>,
    clock: &mut Instant,
    target: f64,
) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        if paused.load(Ordering::Relaxed) {
            if let Some(sink) = sink {
                sink.pause();
            }
            let before = Instant::now();
            std::thread::sleep(Duration::from_millis(10));
            *clock += before.elapsed();
            continue;
        }
        if let Some(sink) = sink {
            sink.play();
        }
        if clock.elapsed().as_secs_f64() >= target {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn play(
    bytes: Vec<u8>,
    mime: &str,
    state: &Mutex<State>,
    paused: &AtomicBool,
    stop: &AtomicBool,
) -> Result<()> {
    if mime.starts_with("audio/") {
        let (_output, handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&handle)?;
        sink.append(rodio::Decoder::new(Cursor::new(bytes))?);
        let mut clock = Instant::now();
        while !sink.empty() {
            if !wait(paused, stop, Some(&sink), &mut clock, 0.) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        return Ok(());
    }
    let mut input = container(bytes)?;
    // Play the first audio and video track, ignoring subtitle/alternate tracks.
    let streams: Vec<_> = [MediaType::Video, MediaType::Audio]
        .into_iter()
        .filter_map(|kind| input.best_stream(kind).cloned())
        .collect();
    if streams.is_empty() {
        bail!("No playable tracks.");
    }
    let output = if streams.iter().any(|s| s.media_type == MediaType::Audio) {
        Some(OutputStream::try_default()?)
    } else {
        None
    };
    let sink = output
        .as_ref()
        .map(|(_, handle)| Sink::try_new(handle))
        .transpose()?;
    let mut clock = Instant::now();
    let mut first_pts = None;
    let mut fallback = 0.;
    let mut started = false;
    walk(&mut input, |stream, frame, packet_pts| {
        if !started {
            clock = Instant::now();
            started = true;
        }
        if !wait(paused, stop, sink.as_ref(), &mut clock, 0.) {
            return Ok(false);
        }
        match frame {
            Frame::Audio(frame) => {
                if let Some(sink) = &sink {
                    sink.append(rodio::buffer::SamplesBuffer::new(
                        frame.layout.channels as u16,
                        frame.sample_rate,
                        samples(&frame)?,
                    ));
                    while sink.len() > 100 {
                        if !wait(paused, stop, Some(sink), &mut clock, 0.) {
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
                let target = (pts - origin).max(0.);
                fallback = pts + 1. / 30.;
                if !wait(paused, stop, sink.as_ref(), &mut clock, target) {
                    return Ok(false);
                }
                let edge = frame.width.max(frame.height).max(1) as f64;
                let scale = (1280. / edge).min(1.);
                let rgb = rumpeg::scale::transform(
                    &frame,
                    PixelFormat::Rgb24,
                    (frame.width as f64 * scale).max(1.) as u32,
                    (frame.height as f64 * scale).max(1.) as u32,
                    rumpeg::scale::FilterMode::Bilinear,
                )?;
                let mut jpeg = Vec::new();
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85).encode(
                    rgb.plane(0)
                        .ok_or_else(|| anyhow!("Missing video pixels"))?,
                    rgb.width,
                    rgb.height,
                    image::ExtendedColorType::Rgb8,
                )?;
                state.lock().unwrap_or_else(|e| e.into_inner()).frame = Some(jpeg);
            }
        }
        Ok(true)
    })?;
    if let Some(sink) = &sink {
        while !sink.empty() {
            if !wait(paused, stop, Some(sink), &mut clock, 0.) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
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
}
