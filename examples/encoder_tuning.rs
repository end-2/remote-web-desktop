//! Reproducible OpenH264 option comparison; includes color conversion, excludes
//! fixture creation and decode verification. Every frame is decoded for validity.
use image::{Rgba, RgbaImage};
use openh264::{
    OpenH264API, Timestamp,
    encoder::{
        BitRate, Complexity, EncoderConfig, FrameRate, IntraFramePeriod, Level, Profile, QpRange,
        RateControlMode, UsageType, VuiConfig,
    },
    formats::{RgbaSliceU8, YUVBuffer, YUVSource},
};
use std::time::Instant;

fn scene(frame: u32, width: u32, height: u32) -> RgbaImage {
    let mut image = RgbaImage::from_fn(width, height, |x, y| {
        if y < 48 || x < 220 {
            Rgba([27, 34, 48, 255])
        } else if y % 28 < 3 && (260..width - 100).contains(&x) {
            Rgba([96 + (x % 71) as u8, 147, 177, 255])
        } else {
            Rgba([236 + (x % 15) as u8, 239, 244, 255])
        }
    });
    let left = 260 + (frame * 13) % (width - 510);
    let top = 90 + (frame * 7) % (height - 280);
    for y in top..top + 170 {
        for x in left..left + 240 {
            image.put_pixel(x, y, Rgba([45, 92 + (y % 89) as u8, 174, 255]));
        }
    }
    image
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let iterations: u32 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(90);
    let width: u32 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1600);
    let height: u32 = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(1038);
    let quality: u32 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(65);
    assert!(
        iterations > 0
            && width >= 640
            && height >= 480
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
            && (1..=100).contains(&quality)
    );
    println!(
        "{width}x{height}, moving desktop fixture, {iterations} frames, quality={quality} target=60; encode includes RGBA->YUV conversion; decode verification excluded"
    );
    println!(
        "OpenH264 forces scene detection on for ScreenContentRealTime; thread/detect columns are requested values."
    );
    println!(
        "mode,threads,scene_detect,qp_floor,mean_ms,p50_ms,p95_ms,average_bytes,decoded_frames,luma_psnr_db"
    );
    for usage in [
        UsageType::ScreenContentRealTime,
        UsageType::CameraVideoRealTime,
    ] {
        for (threads, detect, floor) in [
            (0, true, false),
            (0, false, false),
            (1, false, false),
            (2, false, false),
            (4, false, false),
            (0, false, true),
        ] {
            let max_qp = (46 - quality * 26 / 100) as u8;
            let min_qp = if floor {
                max_qp.saturating_sub(6).max(16)
            } else {
                16
            };
            let config = EncoderConfig::new()
                .usage_type(usage)
                .profile(Profile::Baseline)
                .level(Level::Level_4_2)
                .complexity(Complexity::Low)
                .max_frame_rate(FrameRate::from_hz(60.0))
                .bitrate(BitRate::from_bps(
                    (width * height * 60 / 6).clamp(1_000_000, 30_000_000),
                ))
                .rate_control_mode(RateControlMode::Quality)
                .qp(QpRange::new(min_qp, max_qp))
                .skip_frames(false)
                .adaptive_quantization(false)
                .background_detection(false)
                .intra_frame_period(IntraFramePeriod::from_num_frames(120))
                .vui(VuiConfig::bt601())
                .num_threads(threads)
                .scene_change_detect(detect);
            let mut encoder =
                openh264::encoder::Encoder::with_api_config(OpenH264API::from_source(), config)?;
            let mut yuv = YUVBuffer::new(width as usize, height as usize);
            let mut decoder = openh264::decoder::Decoder::new()?;
            let mut samples = Vec::new();
            let mut bytes = 0;
            let mut squared_error = 0u64;
            for frame in 0..iterations + 5 {
                let rgba = scene(frame, width, height);
                let start = Instant::now();
                yuv.read_rgba8(RgbaSliceU8::new(
                    rgba.as_raw(),
                    (width as usize, height as usize),
                ));
                let data = encoder
                    .encode_at(&yuv, Timestamp::from_millis(u64::from(frame) * 1000 / 60))?
                    .to_vec();
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                if frame >= 5 {
                    samples.push(ms);
                    bytes += data.len();
                }
                let decoded = decoder.decode(&data)?.ok_or("empty decode")?;
                assert_eq!(decoded.dimensions(), (width as usize, height as usize));
                if frame >= 5 {
                    for (actual, expected) in decoded
                        .y()
                        .chunks(decoded.strides().0)
                        .take(height as usize)
                        .zip(yuv.y().chunks(width as usize))
                    {
                        squared_error += actual[..width as usize]
                            .iter()
                            .zip(expected)
                            .map(|(&a, &b)| i64::from(a).abs_diff(i64::from(b)).pow(2))
                            .sum::<u64>();
                    }
                }
            }
            samples.sort_by(f64::total_cmp);
            let mean = samples.iter().sum::<f64>() / samples.len() as f64;
            let mse = squared_error as f64
                / (f64::from(iterations) * f64::from(width) * f64::from(height));
            let psnr = 10.0 * (255.0 * 255.0 / mse).log10();
            println!(
                "{usage:?},{threads},{detect},{min_qp},{mean:.3},{:.3},{:.3},{:.0},{},{psnr:.2}",
                samples[samples.len() / 2],
                samples[(samples.len() * 95).div_ceil(100) - 1],
                bytes as f64 / f64::from(iterations),
                iterations + 5
            );
        }
    }
    Ok(())
}
