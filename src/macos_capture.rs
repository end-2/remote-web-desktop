//! ScreenCaptureKit scales on the OS capture path before pixels reach Rust.
//! The established crate owns all Objective-C/Swift/CoreVideo interaction.
//! Keep only the newest sample: a slow client must never create a capture queue.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use image::RgbaImage;
use screencapturekit::cm::{CMSampleBufferSCExt, SCFrameStatus};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;
use screencapturekit::stream::configuration::color_space;
use screencapturekit::stream::delegate_trait::ErrorHandler;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureSettings {
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Default)]
struct Latest {
    sample: Option<CMSampleBuffer>,
    sequence: u64,
    arrived: Option<Instant>,
    error: Option<String>,
}

static SAMPLE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct Capture {
    stream: SCStream,
    latest: Arc<(Mutex<Latest>, Condvar)>,
    pub settings: CaptureSettings,
    metrics: (Option<u64>, Option<f64>),
}

impl Capture {
    pub fn start(settings: CaptureSettings) -> Result<Self, String> {
        let content = SCShareableContent::get().map_err(|e| e.to_string())?;
        let display = content
            .displays()
            .into_iter()
            .find(|display| display.display_id() == settings.display_id)
            .ok_or_else(|| format!("ScreenCaptureKit display {} not found", settings.display_id))?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|e| e.to_string())?;
        let config = SCStreamConfiguration::new()
            .with_width(settings.width)
            .with_height(settings.height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_scales_to_fit(true)
            .with_shows_cursor(true)
            // Three OS surfaces permit one retained latest sample and capture
            // of the next sample without an unbounded user-space frame queue.
            .with_queue_depth(3)
            .with_minimum_frame_interval(&CMTime::new(1, settings.fps as i32))
            .with_color_space_name(color_space::SRGB)
            .map_err(|e| e.to_string())?;
        let latest = Arc::new((Mutex::new(Latest::default()), Condvar::new()));
        let on_error = latest.clone();
        let delegate = ErrorHandler::new(move |error| {
            if let Ok(mut state) = on_error.0.lock() {
                state.sample = None;
                state.error = Some(error.to_string());
                on_error.1.notify_all();
            }
        });
        let mut stream =
            SCStream::new_with_delegate(&filter, &config, delegate).map_err(|e| e.to_string())?;
        let output = latest.clone();
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                    if kind != SCStreamOutputType::Screen {
                        return;
                    }
                    let status = sample.frame_status();
                    // Apple's capture sample processes only `complete` frames.
                    // Idle/started callbacks are timing notifications, not pixels.
                    if let Ok(mut state) = output.0.lock() {
                        if matches!(
                            status,
                            Some(
                                SCFrameStatus::Blank
                                    | SCFrameStatus::Suspended
                                    | SCFrameStatus::Stopped
                            )
                        ) {
                            state.sample = None;
                            state.error = Some(
                                "ScreenCaptureKit display blank, suspended, or stopped".into(),
                            );
                        } else if status == Some(SCFrameStatus::Complete)
                            && sample.pixel_buffer().is_some()
                        {
                            state.sequence = SAMPLE_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
                            state.arrived = Some(Instant::now());
                            state.sample = Some(sample);
                            state.error = None;
                        }
                        output.1.notify_all();
                    }
                },
                SCStreamOutputType::Screen,
            )
            .map_err(|e| e.to_string())?;
        stream.start_capture().map_err(|e| e.to_string())?;
        tracing::info!(
            backend = "sck",
            width = settings.width,
            height = settings.height,
            fps = settings.fps,
            "native capture started"
        );
        Ok(Self {
            stream,
            latest,
            settings,
            metrics: (None, None),
        })
    }

    pub fn frame(&mut self) -> Result<RgbaImage, String> {
        let state = self.latest.0.lock().map_err(|_| "capture state poisoned")?;
        // This wait occurs only before the first sample. Once running, reading
        // a retained latest frame does not wait for the next display refresh.
        let (state, _) = self
            .latest
            .1
            .wait_timeout_while(state, Duration::from_secs(1), |state| {
                state.sample.is_none() && state.error.is_none()
            })
            .map_err(|_| "capture state poisoned")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        let sample = state
            .sample
            .clone()
            .ok_or("ScreenCaptureKit first frame timed out")?;
        let sequence = state.sequence;
        let arrived = state.arrived;
        drop(state);
        let buffer = sample
            .pixel_buffer()
            .ok_or("capture sample has no pixel buffer")?;
        if buffer.pixel_format() != u32::from_be_bytes(*b"BGRA") {
            return Err("ScreenCaptureKit returned a non-BGRA pixel buffer".into());
        }
        let guard = buffer
            .lock(CVPixelBufferLockFlags::READ_ONLY)
            .map_err(|e| format!("pixel buffer lock: {e}"))?;
        // SAFETY: retaining the delivered sample prevents ScreenCaptureKit from
        // recycling its IOSurface; this code never mutates it. The read-only
        // guard stays locked through the copy and no borrowed memory escapes.
        let bytes = unsafe { guard.as_slice() }.ok_or("capture buffer has no CPU address")?;
        let image = packed_rgba(
            bytes,
            buffer.width(),
            buffer.height(),
            buffer.bytes_per_row(),
        )?;
        self.metrics = (
            Some(sequence),
            arrived.map(|arrived| arrived.elapsed().as_secs_f64() * 1000.0),
        );
        Ok(image)
    }

    /// Metadata of the exact sample last copied by `frame`, not the newest
    /// sample that may have arrived concurrently while encoding it.
    pub fn metrics(&self) -> (Option<u64>, Option<f64>) {
        self.metrics
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Err(error) = self.stream.stop_capture() {
            tracing::warn!(%error, "ScreenCaptureKit stop failed");
        }
    }
}

fn packed_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> Result<RgbaImage, String> {
    if width < 2 || height < 2 || width > 7680 || height > 8192 {
        return Err("invalid capture dimensions".into());
    }
    let row_bytes = width * 4;
    let required = stride
        .checked_mul(height - 1)
        .and_then(|bytes| bytes.checked_add(row_bytes));
    if stride < row_bytes || required.is_none_or(|required| data.len() < required) {
        return Err("invalid capture pixel row stride or buffer size".into());
    }
    let mut rgba = vec![0; row_bytes * height];
    for (source, target) in data
        .chunks(stride)
        .take(height)
        .zip(rgba.chunks_exact_mut(row_bytes))
    {
        for (bgra, rgba) in source[..row_bytes]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(target.as_chunks_mut::<4>().0.iter_mut())
        {
            rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], 255]);
        }
    }
    RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or_else(|| "invalid RGBA capture size".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_bgra_rows_preserve_colors_and_ignore_padding() {
        let bytes = [
            3, 2, 1, 0, 6, 5, 4, 0, 99, 99, 99, 99, 9, 8, 7, 0, 12, 11, 10, 0,
        ];
        let image = packed_rgba(&bytes, 2, 2, 12).unwrap();
        assert_eq!(
            image.as_raw(),
            &[1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
        assert!(packed_rgba(&bytes[..19], 2, 2, 12).is_err());
        assert!(packed_rgba(&bytes, 2, 2, 4).is_err());
    }
}
