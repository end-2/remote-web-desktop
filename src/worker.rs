//! Bounded library workers: native input is never blocked by video encoding.
use image::RgbaImage;
use std::{sync::Arc, time::Instant};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};

use crate::{
    desktop::{CaptureBackend, Desktop, DesktopInfo},
    encoding::{EncodedFrame, Encoder, H264Mode},
    protocol::{InputEvent, StreamSettings},
};

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub struct CapturedFrame {
    pub image: RgbaImage,
    pub capture_ms: f64,
    pub backend: &'static str,
    pub sequence: Option<u64>,
    pub age_ms: Option<f64>,
}

enum DesktopCommand {
    Capture {
        monitor: u32,
        max_width: u32,
        fps: u32,
        reply: Reply<CapturedFrame>,
    },
    Input {
        event: InputEvent,
        monitor: u32,
        reply: Reply<()>,
    },
    Stop {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct DesktopWorker {
    tx: mpsc::Sender<DesktopCommand>,
}

impl DesktopWorker {
    pub async fn start(
        demo: bool,
        capture_backend: CaptureBackend,
        permit: OwnedSemaphorePermit,
    ) -> Result<(Self, DesktopInfo), String> {
        let (tx, mut rx) = mpsc::channel::<DesktopCommand>(32);
        let (ready_tx, ready_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("desktop-native".into())
            .spawn(move || {
                // Hold the exclusive controller lease until all pressed keys have been released.
                let _permit = permit;
                let mut desktop = match Desktop::new(demo, capture_backend) {
                    Ok(desktop) => desktop,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                if ready_tx.send(Ok(desktop.info())).is_err() {
                    return;
                }
                while let Some(command) = rx.blocking_recv() {
                    match command {
                        DesktopCommand::Capture {
                            monitor,
                            max_width,
                            fps,
                            reply,
                        } => {
                            let started = Instant::now();
                            let result =
                                desktop.capture_raw(monitor, max_width, fps).map(|image| {
                                    let (sequence, age_ms) = desktop.capture_metrics();
                                    CapturedFrame {
                                        image,
                                        capture_ms: started.elapsed().as_secs_f64() * 1000.0,
                                        backend: desktop.capture_backend(),
                                        sequence,
                                        age_ms,
                                    }
                                });
                            let _ = reply.send(result);
                        }
                        DesktopCommand::Input {
                            event,
                            monitor,
                            reply,
                        } => {
                            let _ = reply.send(desktop.input(&event, monitor));
                        }
                        DesktopCommand::Stop { reply } => {
                            if let Err(error) = desktop.release_all() {
                                tracing::warn!(%error, "Input release failed");
                            }
                            if let Err(error) = desktop.stop_capture() {
                                tracing::warn!(%error, "Capture shutdown failed");
                            }
                            drop(desktop);
                            drop(_permit);
                            let _ = reply.send(());
                            return;
                        }
                    }
                }
                // Desktop::drop also releases keys when a task is cancelled or a channel closes.
            })
            .map_err(|e| e.to_string())?;
        let info = ready_rx
            .await
            .map_err(|_| "데스크톱 작업 스레드가 종료되었습니다.".to_string())??;
        Ok((Self { tx }, info))
    }

    pub async fn capture(&self, settings: &StreamSettings) -> Result<CapturedFrame, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DesktopCommand::Capture {
                monitor: settings.monitor,
                max_width: settings.max_width,
                fps: settings.fps,
                reply,
            })
            .await
            .map_err(|_| "화면 캡처 작업이 종료되었습니다.")?;
        rx.await
            .map_err(|_| "화면 캡처 작업이 종료되었습니다.".to_string())?
    }

    pub async fn input(&self, event: InputEvent, monitor: u32) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DesktopCommand::Input {
                event,
                monitor,
                reply,
            })
            .await
            .map_err(|_| "입력 작업이 종료되었습니다.")?;
        rx.await
            .map_err(|_| "입력 작업이 종료되었습니다.".to_string())?
    }

    pub async fn stop(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(DesktopCommand::Stop { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }
}

struct EncodeCommand {
    image: RgbaImage,
    settings: StreamSettings,
    epoch: u64,
    reply: Reply<(EncodedFrame, f64)>,
}

#[derive(Clone)]
pub struct EncodingWorker {
    tx: Arc<mpsc::Sender<EncodeCommand>>,
}

impl EncodingWorker {
    pub fn start(
        resize_method: crate::resize::ResizeMethod,
        h264_mode: H264Mode,
    ) -> Result<Self, String> {
        let (tx, mut rx) = mpsc::channel::<EncodeCommand>(1);
        std::thread::Builder::new()
            .name("desktop-encoder".into())
            .spawn(move || {
                let mut current = None;
                let mut encoder: Option<Encoder> = None;
                let mut resizer = crate::resize::Resizer::new(resize_method);
                while let Some(command) = rx.blocking_recv() {
                    let EncodeCommand {
                        image,
                        settings,
                        epoch,
                        reply,
                    } = command;
                    let started = Instant::now();
                    let result = (|| {
                        let image = resizer.resize(image, settings.max_width)?;
                        let key = (
                            epoch,
                            settings.codec,
                            settings.quality,
                            settings.fps,
                            settings.monitor,
                            image.width(),
                            image.height(),
                        );
                        if current != Some(key) {
                            encoder = Some(Encoder::with_h264_mode(
                                settings.codec,
                                settings.quality,
                                settings.fps,
                                h264_mode,
                            )?);
                            current = Some(key);
                        }
                        encoder.as_mut().unwrap().encode(&image)
                    })()
                    .map(|frame| (frame, started.elapsed().as_secs_f64() * 1000.0));
                    let _ = reply.send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx: Arc::new(tx) })
    }

    pub async fn encode(
        &self,
        image: RgbaImage,
        settings: StreamSettings,
        epoch: u64,
    ) -> Result<(EncodedFrame, f64), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(EncodeCommand {
                image,
                settings,
                epoch,
                reply,
            })
            .await
            .map_err(|_| "인코더가 종료되었습니다.")?;
        rx.await
            .map_err(|_| "인코더가 종료되었습니다.".to_string())?
    }
}

pub struct Frame {
    pub encoded: EncodedFrame,
    pub capture_ms: f64,
    pub encode_ms: f64,
    pub capture_backend: &'static str,
    pub capture_sequence: Option<u64>,
    pub capture_age_ms: Option<f64>,
}

pub async fn frame(
    desktop: DesktopWorker,
    encoder: EncodingWorker,
    settings: StreamSettings,
    epoch: u64,
) -> Result<Frame, String> {
    let captured = desktop.capture(&settings).await?;
    let (encoded, encode_ms) = encoder.encode(captured.image, settings, epoch).await?;
    Ok(Frame {
        encoded,
        capture_ms: captured.capture_ms,
        encode_ms,
        capture_backend: captured.backend,
        capture_sequence: captured.sequence,
        capture_age_ms: captured.age_ms,
    })
}
