//! Reuse maintained image/video codecs; keep conversion buffers across frames.
use std::{fmt, time::Instant};

use image::{GenericImageView, Rgba, RgbaImage};
use openh264::{
    OpenH264API, Timestamp,
    encoder::{
        BitRate, Complexity, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level, Profile,
        QpRange, RateControlMode, UsageType, VuiConfig,
    },
    formats::{RgbaSliceU8, YUVBuffer, YUVSource},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Jpeg,
    Image,
    H264,
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Jpeg => "jpeg",
            Self::Image => "image",
            Self::H264 => "h264",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum H264Mode {
    /// Original screen-content search; preserves the comparison baseline.
    Screen,
    /// Faster camera-video search and a quality-dependent quantizer floor.
    #[default]
    Video,
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub key: bool,
    pub codec: Codec,
    /// RFC 6381 AVC codec string derived from this frame's actual SPS.
    pub decoder_config: Option<String>,
}

pub struct Encoder {
    codec: Codec,
    quality: u8,
    fps: u32,
    h264_mode: H264Mode,
    h264: Option<openh264::encoder::Encoder>,
    yuv: Option<YUVBuffer>,
    dimensions: Option<(usize, usize)>,
    frame_number: u64,
}

impl Encoder {
    #[cfg(test)]
    pub fn new(codec: Codec, quality: u8, fps: u32) -> Result<Self, String> {
        Self::with_h264_mode(codec, quality, fps, H264Mode::default())
    }

    pub fn with_h264_mode(
        codec: Codec,
        quality: u8,
        fps: u32,
        h264_mode: H264Mode,
    ) -> Result<Self, String> {
        if !(1..=100).contains(&quality) || !(1..=60).contains(&fps) {
            return Err("quality must be 1..=100 and fps must be 1..=60".into());
        }
        Ok(Self {
            codec,
            quality,
            fps,
            h264_mode,
            h264: None,
            yuv: None,
            dimensions: None,
            frame_number: 0,
        })
    }

    pub fn encode(&mut self, rgba: &RgbaImage) -> Result<EncodedFrame, String> {
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("frame dimensions must be within 1..=8192".into());
        }
        let mut data = Vec::new();
        match self.codec {
            Codec::Jpeg => {
                let mut encoder = jpeg_encoder::Encoder::new(&mut data, self.quality);
                // Match image's 4:2:2 sampling for a useful baseline comparison.
                encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_1);
                encoder
                    .encode(
                        rgba.as_raw(),
                        width as u16,
                        height as u16,
                        jpeg_encoder::ColorType::Rgba,
                    )
                    .map_err(|error| error.to_string())?;
            }
            Codec::Image => {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, self.quality)
                    .encode_image(rgba)
                    .map_err(|error| error.to_string())?;
            }
            Codec::H264 => return self.encode_h264(rgba),
        }
        Ok(EncodedFrame {
            data,
            key: true,
            codec: self.codec,
            decoder_config: None,
        })
    }

    fn encode_h264(&mut self, rgba: &RgbaImage) -> Result<EncodedFrame, String> {
        let dimensions = (rgba.width() as usize, rgba.height() as usize);
        if !dimensions.0.is_multiple_of(2) || !dimensions.1.is_multiple_of(2) {
            return Err("H.264 requires even frame width and height".into());
        }
        // Bound resolution to the capabilities of the configured AVC level.
        if dimensions.0.div_ceil(16) * dimensions.1.div_ceil(16) > 36_864 {
            return Err("H.264 frame exceeds level 5.2 macroblock limit".into());
        }
        if self.dimensions != Some(dimensions) {
            let macroblocks = dimensions.0.div_ceil(16) * dimensions.1.div_ceil(16);
            let level = if macroblocks <= 8704 {
                Level::Level_4_2
            } else {
                Level::Level_5_2
            };
            // The quality scale is not JPEG-equivalent. The fast video policy
            // also raises minimum QP at low quality so rate control cannot keep
            // spending bits on detail that the user has chosen to discard.
            let max_qp = 46 - u32::from(self.quality) * 26 / 100;
            let min_qp = match self.h264_mode {
                H264Mode::Screen => 16,
                H264Mode::Video => max_qp.saturating_sub(6).max(16),
            };
            let target_bitrate = (dimensions.0 as u64 * dimensions.1 as u64 * u64::from(self.fps)
                / 6)
            .clamp(1_000_000, 30_000_000) as u32;
            let config = EncoderConfig::new()
                .usage_type(match self.h264_mode {
                    H264Mode::Screen => UsageType::ScreenContentRealTime,
                    H264Mode::Video => UsageType::CameraVideoRealTime,
                })
                .profile(Profile::Baseline)
                .level(level)
                .complexity(Complexity::Low)
                .max_frame_rate(FrameRate::from_hz(self.fps as f32))
                .bitrate(BitRate::from_bps(target_bitrate))
                .rate_control_mode(RateControlMode::Quality)
                .qp(QpRange::new(min_qp as u8, max_qp as u8))
                .skip_frames(false)
                .scene_change_detect(self.h264_mode == H264Mode::Screen)
                .adaptive_quantization(false)
                .background_detection(false)
                .intra_frame_period(IntraFramePeriod::from_num_frames(self.fps * 2))
                // OpenH264's RGB conversion is BT.601 limited range.
                .vui(VuiConfig::bt601());
            self.h264 = Some(
                openh264::encoder::Encoder::with_api_config(OpenH264API::from_source(), config)
                    .map_err(|error| format!("H.264 encoder: {error}"))?,
            );
            self.yuv = Some(YUVBuffer::new(dimensions.0, dimensions.1));
            self.dimensions = Some(dimensions);
            self.frame_number = 0;
        }
        let yuv = self.yuv.as_mut().ok_or("missing YUV buffer")?;
        yuv.read_rgba8(RgbaSliceU8::new(rgba.as_raw(), dimensions));
        let timestamp = Timestamp::from_millis(self.frame_number * 1000 / u64::from(self.fps));
        let encoded = self
            .h264
            .as_mut()
            .ok_or("missing H.264 encoder")?
            .encode_at(yuv, timestamp)
            .map_err(|error| format!("H.264 encode: {error}"))?;
        let key = encoded.frame_type() == FrameType::IDR;
        let data = encoded.to_vec();
        if data.is_empty() {
            return Err("H.264 encoder produced an empty frame".into());
        }
        self.frame_number += 1;
        let decoder_config = decoder_config_from_annex_b(&data);
        Ok(EncodedFrame {
            data,
            key,
            codec: Codec::H264,
            decoder_config,
        })
    }
}

fn decoder_config_from_annex_b(data: &[u8]) -> Option<String> {
    for unit in openh264::nal_units(data) {
        let payload = unit
            .strip_prefix(&[0, 0, 0, 1])
            .or_else(|| unit.strip_prefix(&[0, 0, 1]))?;
        if payload.len() >= 4 && payload[0] & 0x1f == 7 {
            return Some(format!(
                "avc1.{:02X}{:02X}{:02X}",
                payload[1], payload[2], payload[3]
            ));
        }
    }
    None
}

fn verify_decode(frame: &EncodedFrame, expected: (u32, u32)) -> Result<(), String> {
    let dimensions = match frame.codec {
        Codec::Jpeg | Codec::Image => image::load_from_memory(&frame.data)
            .map_err(|error| format!("JPEG decode validation: {error}"))?
            .dimensions(),
        Codec::H264 => {
            let mut decoder = openh264::decoder::Decoder::new()
                .map_err(|error| format!("H.264 decoder: {error}"))?;
            let decoded = decoder
                .decode(&frame.data)
                .map_err(|error| format!("H.264 decode validation: {error}"))?
                .ok_or("H.264 first frame was not decodable")?;
            let (width, height) = decoded.dimensions();
            (width as u32, height as u32)
        }
    };
    if dimensions != expected {
        return Err(format!("decoded {dimensions:?}, expected {expected:?}"));
    }
    Ok(())
}

fn benchmark_scene(frame: u32) -> RgbaImage {
    let mut image = RgbaImage::from_fn(1280, 720, |x, y| {
        if y < 48 || x < 220 {
            Rgba([27, 34, 48, 255])
        } else if y % 28 < 3 && (260..1120).contains(&x) {
            Rgba([96 + (x % 71) as u8, 147, 177, 255])
        } else {
            Rgba([236 + (x % 15) as u8, 239, 244, 255])
        }
    });
    let left = 260 + (frame * 13) % 700;
    let top = 90 + (frame * 7) % 390;
    for y in top..top + 170 {
        for x in left..left + 240 {
            image.put_pixel(x, y, Rgba([45, 92 + (y % 89) as u8, 174, 255]));
        }
    }
    image
}

/// Codec-only benchmark: conversion and encoding included; capture/network excluded.
pub fn benchmark(iterations: u32) -> Result<(), String> {
    if !(1..=10_000).contains(&iterations) {
        return Err("benchmark iterations must be 1..=10000".into());
    }
    println!(
        "1280x720 RGBA desktop-like moving window; {iterations} frames; quality=70; target=30 fps"
    );
    println!("Encoding/conversion only. Capture, resize, network and display excluded.");
    println!(
        "JPEG paths use 4:2:2; H264 uses 4:2:0 inter prediction. Quality scales are not equivalent."
    );
    println!("codec,avg_ms,p50_ms,p95_ms,avg_bytes,encode_fps,decoded_size");
    for (codec, h264_mode, label) in [
        (Codec::Image, H264Mode::Screen, "image"),
        (Codec::Jpeg, H264Mode::Screen, "jpeg"),
        (Codec::H264, H264Mode::Screen, "h264-screen"),
        (Codec::H264, H264Mode::Video, "h264-video"),
    ] {
        // Warm library/code paths without removing the measured stream's first IDR.
        let mut warmup = Encoder::with_h264_mode(codec, 70, 30, h264_mode)?;
        for frame in 0..3 {
            warmup.encode(&benchmark_scene(frame))?;
        }
        drop(warmup);
        let mut encoder = Encoder::with_h264_mode(codec, 70, 30, h264_mode)?;
        let mut elapsed = Vec::with_capacity(iterations as usize);
        let mut bytes = 0usize;
        for frame in 0..iterations {
            let rgba = benchmark_scene(frame);
            let start = Instant::now();
            let output = encoder.encode(&rgba)?;
            elapsed.push(start.elapsed().as_secs_f64() * 1000.0);
            bytes += output.data.len();
            if frame == 0 {
                verify_decode(&output, rgba.dimensions())?;
            }
        }
        let total: f64 = elapsed.iter().sum();
        elapsed.sort_by(f64::total_cmp);
        let percentile = |percentage: usize| {
            elapsed[(elapsed.len() * percentage).div_ceil(100).saturating_sub(1)]
        };
        println!(
            "{label},{:.3},{:.3},{:.3},{:.0},{:.1},1280x720",
            total / f64::from(iterations),
            percentile(50),
            percentile(95),
            bytes as f64 / f64::from(iterations),
            f64::from(iterations) * 1000.0 / total,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_codecs_produce_decodable_frames() {
        let rgba = RgbaImage::from_fn(128, 72, |x, y| Rgba([x as u8, y as u8, 120, 255]));
        for codec in [Codec::Jpeg, Codec::Image, Codec::H264] {
            let mut encoder = Encoder::new(codec, 70, 30).unwrap();
            let frame = encoder.encode(&rgba).unwrap();
            verify_decode(&frame, rgba.dimensions()).unwrap();
            assert!(frame.key);
            if codec == Codec::H264 {
                assert!(
                    frame
                        .decoder_config
                        .as_ref()
                        .unwrap()
                        .starts_with("avc1.42")
                );
                let mut decoder = openh264::decoder::Decoder::new().unwrap();
                assert!(decoder.decode(&frame.data).unwrap().is_some());
                let delta = encoder.encode(&rgba).unwrap();
                assert!(!delta.key);
                assert!(decoder.decode(&delta.data).unwrap().is_some());
            }
        }
    }

    #[test]
    fn h264_resolution_change_starts_new_stream() {
        let mut encoder = Encoder::new(Codec::H264, 70, 30).unwrap();
        encoder.encode(&RgbaImage::new(128, 72)).unwrap();
        let next = encoder.encode(&RgbaImage::new(160, 90)).unwrap();
        assert!(next.key);
        assert!(next.decoder_config.is_some());
        verify_decode(&next, (160, 90)).unwrap();
        assert!(encoder.encode(&RgbaImage::new(161, 90)).is_err());
    }

    #[test]
    fn both_h264_modes_decode_inter_frames_at_quality_extremes() {
        for mode in [H264Mode::Screen, H264Mode::Video] {
            for quality in [1, 45, 100] {
                let mut encoder = Encoder::with_h264_mode(Codec::H264, quality, 60, mode).unwrap();
                let mut decoder = openh264::decoder::Decoder::new().unwrap();
                for frame in 0..8 {
                    let rgba = RgbaImage::from_fn(160, 90, |x, y| {
                        Rgba([(x + frame * 9) as u8, y as u8, 120, 255])
                    });
                    let encoded = encoder.encode(&rgba).unwrap();
                    assert_eq!(encoded.key, frame == 0);
                    assert_eq!(
                        decoder.decode(&encoded.data).unwrap().unwrap().dimensions(),
                        (160, 90)
                    );
                }
            }
        }
    }
}
