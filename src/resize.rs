//! Compare established resampling libraries; no custom pixel resampling code.
use std::{hint::black_box, time::Instant};

use clap::ValueEnum;
use fast_image_resize as fir;
use image::{Rgba, RgbaImage, imageops::FilterType};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ResizeMethod {
    /// image crate's scalar triangle/bilinear filter.
    Image,
    /// fast_image_resize's runtime-selected SIMD bilinear filter.
    Fast,
    /// fast_image_resize nearest-neighbor sampling: fastest, with jagged edges.
    Nearest,
}

pub struct Resizer {
    method: ResizeMethod,
    fast: fir::Resizer,
}

impl Resizer {
    pub fn new(method: ResizeMethod) -> Self {
        Self {
            method,
            fast: fir::Resizer::new(),
        }
    }

    pub fn resize(&mut self, source: RgbaImage, max_width: u32) -> Result<RgbaImage, String> {
        if !(64..=7680).contains(&max_width) || source.width() < 2 || source.height() < 2 {
            return Err("화면 크기 또는 최대 너비가 허용 범위를 벗어났습니다.".into());
        }
        let scale = (max_width as f64 / source.width() as f64)
            .min(8192.0 / source.height() as f64)
            .min(1.0);
        let width = ((source.width() as f64 * scale).round() as u32).max(2);
        let height = ((source.height() as f64 * scale).round() as u32).max(2);
        let image = if source.dimensions() == (width, height) {
            source
        } else {
            match self.method {
                ResizeMethod::Image => {
                    image::imageops::resize(&source, width, height, FilterType::Triangle)
                }
                ResizeMethod::Fast | ResizeMethod::Nearest => {
                    let mut destination = RgbaImage::new(width, height);
                    // Screen pixels are opaque. Disable alpha premultiplication
                    // to match the image crate's comparison filter exactly.
                    let algorithm = match self.method {
                        ResizeMethod::Nearest => fir::ResizeAlg::Nearest,
                        _ => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
                    };
                    let options = fir::ResizeOptions::new()
                        .resize_alg(algorithm)
                        .use_alpha(false);
                    self.fast
                        .resize(&source, &mut destination, &options)
                        .map_err(|e| format!("화면 크기 조정 실패: {e}"))?;
                    destination
                }
            }
        };
        // H.264 4:2:0 requires even dimensions. Keep the aspect-preserving
        // filter result and crop at most one edge pixel, with no second resize.
        let even = (image.width() & !1, image.height() & !1);
        Ok(if image.dimensions() == even {
            image
        } else {
            image::imageops::crop_imm(&image, 0, 0, even.0, even.1).to_image()
        })
    }
}

fn benchmark_source() -> RgbaImage {
    RgbaImage::from_fn(1920, 1080, |x, y| {
        let color = if x < 280 {
            [25, 34, 47]
        } else if (350..1840).contains(&x) && (220..930).contains(&y) {
            let ink = y % 32 < 3 && x % 180 < 130;
            if ink { [188, 206, 224] } else { [38, 50, 67] }
        } else {
            [
                (x / 12 % 128 + 32) as u8,
                (y / 8 % 128 + 32) as u8,
                ((x + y) / 16 % 128 + 48) as u8,
            ]
        };
        Rgba([color[0], color[1], color[2], 255])
    })
}

pub fn benchmark(iterations: u32) -> Result<(), String> {
    let count = iterations.clamp(1, 1000);
    let source = benchmark_source();
    let mut outputs = Vec::new();
    println!(
        "Resize benchmark: identical opaque RGBA 1920x1080 -> 1280x720, {count} iterations; Image/Fast bilinear, Nearest lower quality"
    );
    println!("method,mean_ms,p50_ms,p95_ms");
    for method in [
        ResizeMethod::Image,
        ResizeMethod::Fast,
        ResizeMethod::Nearest,
    ] {
        let mut resizer = Resizer::new(method);
        for _ in 0..3 {
            black_box(resizer.resize(source.clone(), 1280)?);
        }
        let mut samples = Vec::with_capacity(count as usize);
        let mut output = None;
        for _ in 0..count {
            // Cloning the common immutable fixture is setup, not resize work.
            let input = source.clone();
            let start = Instant::now();
            let resized = resizer.resize(input, 1280)?;
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            if resized.dimensions() != (1280, 720) {
                return Err("잘못된 벤치마크 출력 크기입니다.".into());
            }
            output = Some(black_box(resized));
        }
        samples.sort_by(f64::total_cmp);
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let percentile = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
        println!(
            "{method:?},{mean:.3},{:.3},{:.3}",
            percentile(0.5),
            percentile(0.95)
        );
        outputs.push(output.unwrap());
    }
    let mae = mean_absolute_rgb_error(&outputs[0], &outputs[1]);
    println!(
        "Output validation: equal dimensions; RGB mean absolute error = {mae:.6}/255 (rounding differences)"
    );
    if mae > 1.0 {
        return Err(format!(
            "리사이즈 구현 간 평균 픽셀 오차가 너무 큽니다: {mae}"
        ));
    }
    println!(
        "Nearest quality tradeoff: RGB mean absolute error vs bilinear = {:.6}/255; equal dimensions",
        mean_absolute_rgb_error(&outputs[0], &outputs[2])
    );
    Ok(())
}

fn mean_absolute_rgb_error(left: &RgbaImage, right: &RgbaImage) -> f64 {
    assert_eq!(left.dimensions(), right.dimensions());
    let difference: u64 = left
        .pixels()
        .zip(right.pixels())
        .map(|(a, b)| {
            (0..3)
                .map(|channel| a[channel].abs_diff(b[channel]) as u64)
                .sum::<u64>()
        })
        .sum();
    difference as f64 / (left.width() as f64 * left.height() as f64 * 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries_agree_on_bilinear_output_and_dimensions() {
        let source = benchmark_source();
        let baseline = Resizer::new(ResizeMethod::Image)
            .resize(source.clone(), 1280)
            .unwrap();
        let optimized = Resizer::new(ResizeMethod::Fast)
            .resize(source, 1280)
            .unwrap();
        assert_eq!(baseline.dimensions(), (1280, 720));
        assert_eq!(optimized.dimensions(), baseline.dimensions());
        assert!(mean_absolute_rgb_error(&baseline, &optimized) < 1.0);
    }

    #[test]
    fn preserves_aspect_crops_odd_dimensions_and_bounds_tall_displays() {
        for method in [
            ResizeMethod::Image,
            ResizeMethod::Fast,
            ResizeMethod::Nearest,
        ] {
            let mut resizer = Resizer::new(method);
            assert_eq!(
                resizer
                    .resize(RgbaImage::new(1440, 900), 721)
                    .unwrap()
                    .dimensions(),
                (720, 450)
            );
            assert_eq!(
                resizer
                    .resize(RgbaImage::new(641, 481), 1600)
                    .unwrap()
                    .dimensions(),
                (640, 480)
            );
            assert_eq!(
                resizer
                    .resize(RgbaImage::new(100, 10000), 1600)
                    .unwrap()
                    .dimensions(),
                (82, 8192)
            );
            assert!(resizer.resize(RgbaImage::new(1, 10), 1000).is_err());
            assert!(resizer.resize(RgbaImage::new(100, 100), 0).is_err());
        }
    }
}
