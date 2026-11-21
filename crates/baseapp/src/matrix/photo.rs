//! Shrinks still photos to JPEG before they are sent.
//!
//! JPEG and PNG are decoded, scaled, and encoded with [rumpeg](https://crates.io/crates/rumpeg).
//! WebP and BMP are decoded with the `image` crate, then encoded with rumpeg. Rumpeg's JPEG
//! encoder uses its built-in quality. Animated GIF and animated WebP are left as they are.

use rumpeg::scale::FilterMode;
use rumpeg::{
    Buffer, CodecId, CodecParams, Frame, Packet, PixelFormat, Rational, VideoFrame, VideoParams,
};

/// How large a compressed photo may be, measured on its longest side.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PhotoSize {
    Small,
    Balanced,
    Large,
}

impl PhotoSize {
    pub const ALL: [Self; 3] = [Self::Small, Self::Balanced, Self::Large];

    pub fn id(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Balanced => "balanced",
            Self::Large => "large",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "small" => Self::Small,
            "large" => Self::Large,
            _ => Self::Balanced,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "1280 px",
            Self::Balanced => "1920 px",
            Self::Large => "2560 px",
        }
    }

    pub fn longest(self) -> u32 {
        match self {
            Self::Small => 1280,
            Self::Balanced => 1920,
            Self::Large => 2560,
        }
    }
}

pub(super) struct Prepared {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// True when this file can be turned into a smaller JPEG.
pub(super) fn can_compress(bytes: &[u8]) -> bool {
    is_jpeg(bytes) || is_png(bytes) || is_bmp(bytes) || (is_webp(bytes) && !animated_webp(bytes))
}

/// Returns a JPEG limited to `longest` pixels on the longest side.
///
/// The original bytes are returned when the file is not a still photo, compression fails, or the
/// JPEG would be larger without actually shrinking the picture.
pub(super) fn compress(name: String, bytes: Vec<u8>, longest: u32) -> Prepared {
    if !can_compress(&bytes) {
        return Prepared { name, bytes };
    }
    let Some((jpeg, scaled)) = shrink(&bytes, longest) else {
        log::warn!("Could not compress {name}; sending the original.");
        return Prepared { name, bytes };
    };
    if !scaled && jpeg.len() >= bytes.len() {
        return Prepared { name, bytes };
    }
    Prepared {
        name: jpeg_name(&name),
        bytes: jpeg,
    }
}

fn shrink(bytes: &[u8], longest: u32) -> Option<(Vec<u8>, bool)> {
    let frame = decode(bytes)?;
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let (frame, scaled) = fit(frame, longest)?;
    let jpeg = encode(&frame)?;
    if jpeg.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some((jpeg, scaled))
    } else {
        None
    }
}

fn decode(bytes: &[u8]) -> Option<VideoFrame> {
    rumpeg::init();
    if is_jpeg(bytes) {
        decode_rumpeg(bytes, CodecId::Mjpeg)
    } else if is_png(bytes) {
        decode_rumpeg(bytes, CodecId::Png)
    } else if is_bmp(bytes) || is_webp(bytes) {
        decode_still(bytes)
    } else {
        None
    }
}

fn decode_rumpeg(bytes: &[u8], codec: CodecId) -> Option<VideoFrame> {
    let params = CodecParams::video_codec(
        codec,
        VideoParams {
            pix_fmt: PixelFormat::Rgb24,
            width: 0,
            height: 0,
            frame_rate: Rational::one(),
            sample_aspect_ratio: Rational::one(),
        },
    );
    let mut decoder = rumpeg::codec::DecoderContext::open(&params)
        .map_err(|error| log::warn!("Could not open a photo decoder: {error}"))
        .ok()?;
    let frames = decoder
        .decode(&Packet::new(Buffer::from_slice(bytes)))
        .map_err(|error| log::warn!("Could not decode a photo: {error}"))
        .ok()?;
    frames.into_iter().find_map(|frame| match frame {
        Frame::Video(frame) => Some(frame),
        Frame::Audio(_) => None,
    })
}

fn decode_still(bytes: &[u8]) -> Option<VideoFrame> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| log::warn!("Could not read a photo: {error}"))
        .ok()?;
    let rgb = on_white(image);
    let (width, height) = rgb.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let mut frame = VideoFrame::alloc(PixelFormat::Rgb24, width, height);
    frame.plane_mut(0)?.copy_from_slice(rgb.as_raw());
    Some(frame)
}

fn on_white(image: image::DynamicImage) -> image::RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let rgba = image.into_rgba8();
    image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let pixel = rgba.get_pixel(x, y);
        let alpha = pixel[3] as u16;
        let blend = |channel: u8| ((channel as u16 * alpha + 255 * (255 - alpha)) / 255) as u8;
        image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])])
    })
}

fn fit(frame: VideoFrame, longest: u32) -> Option<(VideoFrame, bool)> {
    let edge = frame.width.max(frame.height);
    if longest == 0 || edge <= longest {
        return Some((frame, false));
    }
    let scale = longest as f64 / edge as f64;
    let width = ((frame.width as f64 * scale).round() as u32).max(1);
    let height = ((frame.height as f64 * scale).round() as u32).max(1);
    if width == frame.width && height == frame.height {
        return Some((frame, false));
    }
    match rumpeg::scale::transform(
        &frame,
        PixelFormat::Rgb24,
        width,
        height,
        FilterMode::Bilinear,
    ) {
        Ok(scaled) => Some((scaled, true)),
        Err(error) => {
            log::warn!("Could not resize a photo: {error}");
            None
        }
    }
}

fn encode(frame: &VideoFrame) -> Option<Vec<u8>> {
    let params = CodecParams::video_codec(
        CodecId::Mjpeg,
        VideoParams {
            pix_fmt: PixelFormat::Rgb24,
            width: frame.width,
            height: frame.height,
            frame_rate: Rational::one(),
            sample_aspect_ratio: Rational::one(),
        },
    );
    let mut encoder = rumpeg::codec::EncoderContext::open(&params)
        .map_err(|error| log::warn!("Could not open the photo encoder: {error}"))
        .ok()?;
    let packets = encoder
        .encode(&Frame::Video(frame.clone()))
        .map_err(|error| log::warn!("Could not encode a photo: {error}"))
        .ok()?;
    let jpeg = packets.into_iter().fold(Vec::new(), |mut bytes, packet| {
        bytes.extend_from_slice(packet.data.as_slice());
        bytes
    });
    (!jpeg.is_empty()).then_some(jpeg)
}

fn jpeg_name(name: &str) -> String {
    let trimmed = name.trim();
    let stem = match trimmed.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|glyph| glyph.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => trimmed,
    };
    if stem.is_empty() {
        "photo.jpg".to_owned()
    } else {
        format!("{stem}.jpg")
    }
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G'])
}

fn is_bmp(bytes: &[u8]) -> bool {
    bytes.starts_with(b"BM")
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn animated_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 21 && &bytes[12..16] == b"VP8X" && bytes[20] & 0x02 != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder as _;

    #[test]
    fn opaque_photos_reuse_the_rgb_buffer_and_alpha_blends_on_white() {
        let rgb = image::RgbImage::from_pixel(2, 2, image::Rgb([12, 34, 56]));
        let pixels = rgb.as_ptr();
        let result = on_white(image::DynamicImage::ImageRgb8(rgb));
        assert_eq!(result.as_ptr(), pixels);
        assert_eq!(result.get_pixel(0, 0).0, [12, 34, 56]);
        let rgba = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 128]));
        let result = on_white(image::DynamicImage::ImageRgba8(rgba));
        assert_eq!(result.get_pixel(0, 0).0, [255, 127, 127]);
    }

    #[test]
    fn compresses_png_within_the_longest_side() {
        let image = image::RgbImage::from_fn(80, 40, |x, y| {
            image::Rgb([(x * 3) as u8, (y * 5) as u8, 90])
        });
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(image.as_raw(), 80, 40, image::ExtendedColorType::Rgb8)
            .unwrap();

        let prepared = compress("snap.png".into(), png, 32);
        assert_eq!(prepared.name, "snap.jpg");
        assert!(prepared.bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
        let (width, height) = jpeg_size(&prepared.bytes).unwrap();
        assert_eq!((width, height), (32, 16));
    }

    #[test]
    fn leaves_gif_and_animated_webp_unchanged() {
        let gif = b"GIF89ahello".to_vec();
        let prepared = compress("anim.gif".into(), gif.clone(), 1280);
        assert_eq!(prepared.name, "anim.gif");
        assert_eq!(prepared.bytes, gif);

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.resize(21, 0);
        webp[20] = 0x02;
        assert!(!can_compress(&webp));
    }

    fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
        let mut index = 2;
        while index + 8 < bytes.len() {
            if bytes[index] != 0xFF {
                return None;
            }
            while index < bytes.len() && bytes[index] == 0xFF {
                index += 1;
            }
            let marker = *bytes.get(index)?;
            index += 1;
            if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
                continue;
            }
            let len = u16::from_be_bytes([*bytes.get(index)?, *bytes.get(index + 1)?]) as usize;
            if matches!(
                marker,
                0xC0 | 0xC1
                    | 0xC2
                    | 0xC3
                    | 0xC5
                    | 0xC6
                    | 0xC7
                    | 0xC9
                    | 0xCA
                    | 0xCB
                    | 0xCD
                    | 0xCE
                    | 0xCF
            ) {
                return Some((
                    u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32,
                    u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]) as u32,
                ));
            }
            index += len;
        }
        None
    }
}
