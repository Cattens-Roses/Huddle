//! Ogg Opus voice messages: Rodio's Ogg decoder only handles Vorbis.
use std::{io::Cursor, time::Duration};

use anyhow::{Result, anyhow, bail};
use ogg::reading::PacketReader;
use rodio::Source;
use rumpeg::codec::DecoderContext;
use rumpeg::{
    AudioParams, Buffer, ChannelLayout, CodecId, CodecParams, Frame, Packet, SampleFormat,
};

const RATE: u32 = 48_000;

pub(super) fn is_opus(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
        && PacketReader::new(Cursor::new(bytes))
            .read_packet()
            .ok()
            .flatten()
            .is_some_and(|packet| packet.data.starts_with(b"OpusHead"))
}

pub(super) struct OggOpus {
    packets: std::vec::IntoIter<ogg::Packet>,
    decoder: DecoderContext,
    channels: u16,
    skip: usize,
    remaining: usize,
    gain: f32,
    duration: Duration,
    pending: std::vec::IntoIter<f32>,
    error: Option<String>,
}

impl OggOpus {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        let mut reader = PacketReader::new(Cursor::new(bytes));
        let head = reader.read_packet_expected()?;
        let data = &head.data;
        if data.len() < 19 || !data.starts_with(b"OpusHead") || data[8] == 0 || data[8] > 15 {
            bail!("Invalid Opus header");
        }
        let channels = data[9] as u16;
        if !(1..=2).contains(&channels) || data[18] != 0 {
            bail!("Unsupported Opus channel mapping");
        }
        let pre_skip = u16::from_le_bytes([data[10], data[11]]) as usize;
        let gain = 10_f32.powf(i16::from_le_bytes([data[16], data[17]]) as f32 / (256. * 20.));
        let serial = head.stream_serial();
        let tags = reader.read_packet_expected()?;
        if tags.stream_serial() != serial || !tags.data.starts_with(b"OpusTags") {
            bail!("Missing Opus tags");
        }
        let mut packets = Vec::new();
        let mut end = None;
        while let Some(packet) = reader.read_packet()? {
            if packet.stream_serial() != serial {
                bail!("Unsupported multiplexed or chained Opus stream");
            }
            if packet.last_in_stream() {
                end = Some(packet.absgp_page());
            }
            packets.push(packet);
        }
        let end = end
            .filter(|end| *end != u64::MAX)
            .ok_or_else(|| anyhow!("Incomplete Opus stream"))?;
        let frames = end
            .checked_sub(pre_skip as u64)
            .filter(|frames| *frames > 0)
            .ok_or_else(|| anyhow!("No Opus audio samples"))?;
        let remaining = usize::try_from(frames)?
            .checked_mul(channels as usize)
            .ok_or_else(|| anyhow!("Opus stream is too long"))?;
        rumpeg::init();
        let decoder = DecoderContext::open(&CodecParams::audio_codec(
            CodecId::Opus,
            AudioParams {
                sample_rate: RATE,
                layout: ChannelLayout::new(channels),
                sample_fmt: SampleFormat::S16,
                frame_size: 0,
            },
        ))?;
        let mut source = Self {
            packets: packets.into_iter(),
            decoder,
            channels,
            skip: pre_skip * channels as usize,
            remaining,
            gain,
            duration: Duration::from_secs_f64(frames as f64 / RATE as f64),
            pending: Vec::new().into_iter(),
            error: None,
        };
        source.fill()?;
        Ok(source)
    }

    fn fill(&mut self) -> Result<()> {
        while self.remaining > 0 {
            let packet = self
                .packets
                .next()
                .ok_or_else(|| anyhow!("Truncated Opus audio"))?;
            let mut decoded = Vec::new();
            for frame in self
                .decoder
                .decode(&Packet::new(Buffer::from_vec(packet.data)))?
            {
                if let Frame::Audio(frame) = frame {
                    decoded.extend(super::samples(&frame)?);
                }
            }
            let skip = self.skip.min(decoded.len());
            self.skip -= skip;
            let take = (decoded.len() - skip).min(self.remaining);
            self.remaining -= take;
            self.pending = decoded
                .into_iter()
                .skip(skip)
                .take(take)
                .map(|sample| sample * self.gain)
                .collect::<Vec<_>>()
                .into_iter();
            if self.pending.len() > 0 {
                break;
            }
        }
        Ok(())
    }

    pub fn validate(mut self) -> Result<usize> {
        let count = self.by_ref().count();
        if let Some(error) = self.error {
            bail!(error);
        }
        if count == 0 {
            bail!("No decoded Opus samples");
        }
        Ok(count)
    }
}

impl Iterator for OggOpus {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if let Some(sample) = self.pending.next() {
            return Some(sample);
        }
        if self.error.is_some() || self.remaining == 0 {
            return None;
        }
        if let Err(error) = self.fill() {
            self.error = Some(error.to_string());
            return None;
        }
        self.pending.next()
    }
}

impl Source for OggOpus {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogg::writing::{PacketWriteEndInfo, PacketWriter};

    const VOICE: &[u8] = include_bytes!("../../../tests/media/voice.ogg");

    fn rewrite(mut change: impl FnMut(&mut ogg::Packet)) -> Vec<u8> {
        let mut reader = PacketReader::new(Cursor::new(VOICE));
        let mut writer = PacketWriter::new(Vec::new());
        while let Some(mut packet) = reader.read_packet().unwrap() {
            change(&mut packet);
            let end = if packet.last_in_stream() {
                PacketWriteEndInfo::EndStream
            } else if packet.last_in_page() {
                PacketWriteEndInfo::EndPage
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            let serial = packet.stream_serial();
            let granule = packet.absgp_page();
            writer
                .write_packet(packet.data.into_boxed_slice(), serial, end, granule)
                .unwrap();
        }
        writer.into_inner()
    }

    #[test]
    fn header_gain_is_applied() {
        let quieter = rewrite(|packet| {
            if packet.data.starts_with(b"OpusHead") {
                packet.data[16..18].copy_from_slice(&(-1536_i16).to_le_bytes());
            }
        });
        let normal = OggOpus::new(VOICE.to_vec()).unwrap();
        let quieter = OggOpus::new(quieter).unwrap();
        for (normal, quiet) in normal.zip(quieter) {
            assert!((quiet - normal * 10_f32.powf(-6. / 20.)).abs() < 0.000001);
        }
    }

    #[test]
    fn truncated_and_corrupt_voice_messages_are_rejected() {
        assert!(OggOpus::new(VOICE[..VOICE.len() - 10].to_vec()).is_err());
        let invalid_header = rewrite(|packet| {
            if packet.data.starts_with(b"OpusHead") {
                packet.data[9] = 0;
            }
        });
        assert!(OggOpus::new(invalid_header).is_err());
        let corrupt_end = rewrite(|packet| {
            if packet.last_in_stream() {
                packet.data = vec![3];
            }
        });
        assert!(OggOpus::new(corrupt_end).unwrap().validate().is_err());
    }
}
