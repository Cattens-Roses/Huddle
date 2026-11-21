//! Select audio explicitly: MP4's first track is often video.
use anyhow::{Result, anyhow};
use rodio::Source;
use std::{io::Cursor, time::Duration};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{Decoder, DecoderOptions},
    errors::Error,
    formats::{FormatOptions, FormatReader},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    probe::Hint,
};

pub(super) struct Mp4Audio {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
    channels: u16,
    rate: u32,
    duration: Option<Duration>,
    pending: std::vec::IntoIter<f32>,
    pub error: Option<String>,
}

impl Mp4Audio {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        let stream = MediaSourceStream::new(
            Box::new(Cursor::new(bytes)),
            MediaSourceStreamOptions::default(),
        );
        let reader = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )?
            .format;
        let track = reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.sample_rate.is_some())
            .ok_or_else(|| anyhow!("No audio track in MP4"))?;
        let rate = track.codec_params.sample_rate.unwrap();
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(1);
        if rate == 0 || channels == 0 {
            return Err(anyhow!("Invalid audio parameters"));
        }
        let duration = track
            .codec_params
            .n_frames
            .map(|frames| Duration::from_secs_f64(frames as f64 / rate as f64));
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())?;
        let mut source = Self {
            track: track.id,
            channels,
            rate,
            duration,
            reader,
            decoder,
            pending: Vec::new().into_iter(),
            error: None,
        };
        if !source.fill()? {
            return Err(anyhow!("No decoded audio samples"));
        }
        Ok(source)
    }

    fn fill(&mut self) -> Result<bool> {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(error) => return Err(error.into()),
            };
            if packet.track_id() != self.track {
                continue;
            }
            let decoded = self.decoder.decode(&packet)?;
            self.channels = decoded.spec().channels.count() as u16;
            self.rate = decoded.spec().rate;
            let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
            samples.copy_interleaved_ref(decoded);
            self.pending = samples.samples().to_vec().into_iter();
            if self.pending.len() > 0 {
                return Ok(true);
            }
        }
    }

    pub fn validate(mut self) -> Result<usize> {
        let count = self.by_ref().count();
        if let Some(error) = self.error {
            return Err(anyhow!(error));
        }
        if count == 0 {
            return Err(anyhow!("No decoded audio samples"));
        }
        Ok(count)
    }
}

impl Iterator for Mp4Audio {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if let Some(sample) = self.pending.next() {
            return Some(sample);
        }
        if self.error.is_some() {
            return None;
        }
        match self.fill() {
            Ok(true) => self.pending.next(),
            Ok(false) => None,
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }
}

impl Source for Mp4Audio {
    fn current_frame_len(&self) -> Option<usize> {
        // This track keeps the same sample rate and channel count through EOF.
        // An empty packet buffer is refilled by next(); it is not the end of
        // the source. Some(0) makes Rodio stop skip_duration at that boundary.
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
}
