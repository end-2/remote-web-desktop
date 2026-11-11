use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, State, WebSocketUpgrade,
        ws::{CloseFrame, Message},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::JoinHandle,
};

use crate::{
    auth::{self, Auth},
    encoding::Codec,
    protocol::{ControlMessage, InputEvent, StreamSettings},
    worker::{self, DesktopWorker, EncodingWorker, Frame},
};

pub struct AppState {
    pub auth: Auth,
    pub public_origin: Option<String>,
    pub secure_cookie: bool,
    pub demo: bool,
    pub initial_codec: Codec,
    pub resize_method: crate::resize::ResizeMethod,
    pub capture_backend: crate::desktop::CaptureBackend,
    pub h264_mode: crate::encoding::H264Mode,
    pub controller: Arc<Semaphore>,
    pub shutdown: watch::Receiver<bool>,
    pub http3: Option<Arc<crate::http3::Http3>>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/static/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/static/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/session", get(session))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/transport", post(transport_config))
        .route("/ws", get(upgrade))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
        .with_state(state)
}

async fn security_headers(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let http3_origin = state.http3.as_ref().and_then(|server| {
        crate::http3::origin(
            request.headers(),
            state.public_origin.as_deref(),
            server.port(),
        )
    });
    let connect = http3_origin
        .map(|origin| format!("'self' {origin}"))
        .unwrap_or_else(|| "'self'".into());
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy", HeaderValue::from_str(&format!("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob: data:; connect-src {connect}; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'")).expect("validated CSP origin"));
    response
}

fn error(status: StatusCode, detail: &str) -> Response {
    (status, Json(json!({"detail": detail}))).into_response()
}

async fn session(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Json<Value> {
    Json(
        json!({"authenticated": auth::cookie_token(&headers).is_some_and(|token| state.auth.valid(&token)), "http3_available":state.http3.is_some()}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    password: String,
}

async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Response {
    if !auth::same_origin(&headers, state.public_origin.as_deref()) {
        return error(
            StatusCode::FORBIDDEN,
            "같은 사이트에서만 로그인할 수 있습니다.",
        );
    }
    if !state.auth.allow_attempt(peer.ip()) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "로그인 시도가 너무 많습니다. 1분 뒤 다시 시도하세요.",
        );
    }
    if body.password.len() > 1024 {
        return error(StatusCode::BAD_REQUEST, "비밀번호가 너무 깁니다.");
    }
    let Some(token) = state.auth.login(&body.password) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "접속 비밀번호가 올바르지 않습니다.",
        );
    };
    // Re-login from the same browser revokes the previous cookie/session.
    if let Some(previous) = auth::cookie_token(&headers) {
        state.auth.revoke(&previous);
    }
    let secure = if state.secure_cookie { "; Secure" } else { "" };
    let cookie = format!(
        "{}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{}",
        auth::COOKIE,
        auth::SESSION_SECONDS,
        secure
    );
    ([(header::SET_COOKIE, cookie)], Json(json!({"ok": true}))).into_response()
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !auth::same_origin(&headers, state.public_origin.as_deref()) {
        return error(StatusCode::FORBIDDEN, "잘못된 요청 출처입니다.");
    }
    if let Some(token) = auth::cookie_token(&headers) {
        state.auth.revoke(&token);
    }
    let secure = if state.secure_cookie { "; Secure" } else { "" };
    (
        [(
            header::SET_COOKIE,
            format!(
                "{}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}",
                auth::COOKIE
            ),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

async fn transport_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !auth::same_origin(&headers, state.public_origin.as_deref()) {
        return error(StatusCode::FORBIDDEN, "잘못된 요청 출처입니다.");
    }
    let Some(token) = auth::cookie_token(&headers).filter(|token| state.auth.valid(token)) else {
        return error(StatusCode::UNAUTHORIZED, "로그인이 필요합니다.");
    };
    let Some(server) = &state.http3 else {
        return Json(json!({"mode":"websocket"})).into_response();
    };
    let Some(endpoint_origin) =
        crate::http3::origin(&headers, state.public_origin.as_deref(), server.port())
    else {
        return error(StatusCode::BAD_REQUEST, "잘못된 서버 주소입니다.");
    };
    let origin = headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_owned();
    match server.bootstrap(token, origin, endpoint_origin, &state.auth) {
        Some(config) => Json(config).into_response(),
        None => error(StatusCode::TOO_MANY_REQUESTS, "잠시 후 다시 연결하세요."),
    }
}

async fn upgrade(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !auth::same_origin(&headers, state.public_origin.as_deref()) {
        return error(StatusCode::FORBIDDEN, "잘못된 WebSocket 출처입니다.");
    }
    let token = auth::cookie_token(&headers).filter(|token| state.auth.valid(token));
    ws.max_message_size(16 * 1024)
        .max_frame_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| {
            connection(crate::transport::Socket::websocket(socket), state, token)
        })
}

async fn close(socket: &mut crate::transport::Socket, code: u16, message: &str) {
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = socket
            .send(Message::Text(
                json!({"type": "error", "message": message, "recoverable": false})
                    .to_string()
                    .into(),
            ))
            .await;
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: message.chars().take(30).collect::<String>().into(),
            })))
            .await;
        socket.close().await;
    })
    .await;
}

fn send_json(tx: &mpsc::Sender<Message>, value: Value) -> Result<(), String> {
    tx.try_send(Message::Text(value.to_string().into()))
        .map_err(|_| "클라이언트의 수신 속도가 너무 느립니다.".into())
}

pub(crate) async fn connection(
    mut socket: crate::transport::Socket,
    state: Arc<AppState>,
    token: Option<String>,
) {
    let Some(token) = token else {
        close(&mut socket, 4401, "로그인이 필요합니다.").await;
        return;
    };
    let Ok(permit) = state.controller.clone().try_acquire_owned() else {
        close(
            &mut socket,
            4409,
            "다른 클라이언트가 제어 중입니다. 기존 연결을 종료하세요.",
        )
        .await;
        return;
    };
    let (desktop, info) =
        match DesktopWorker::start(state.demo, state.capture_backend, permit).await {
            Ok(result) => result,
            Err(message) => {
                close(&mut socket, 4500, &message).await;
                return;
            }
        };
    let encoder = match EncodingWorker::start(state.resize_method, state.h264_mode) {
        Ok(encoder) => encoder,
        Err(message) => {
            close(&mut socket, 4500, &message).await;
            desktop.stop().await;
            return;
        }
    };
    let monitors: Vec<u32> = info.monitors.iter().map(|m| m.id).collect();
    let mut settings = StreamSettings {
        monitor: monitors[0],
        codec: state.initial_codec,
        ..Default::default()
    };
    let hello = json!({"type":"hello", "hostname":info.hostname, "platform":info.platform, "mode":info.mode, "monitors":info.monitors, "settings":settings, "max_in_flight":2, "transport":socket.name()});
    let (mut sink, mut source) = socket.split();
    let (tx, mut output) = mpsc::channel::<Message>(8);
    let mut writer = tokio::spawn(async move {
        while let Some(message) = output.recv().await {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(3), sink.send(message)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
    });
    let _ = send_json(&tx, hello);
    let mut shutdown = state.shutdown.clone();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next_frame = Instant::now();
    let mut pending: Option<JoinHandle<Result<Frame, String>>> = None;
    let mut in_flight: VecDeque<Instant> = VecDeque::new();
    let mut requested: Option<StreamSettings> = None;
    let mut last_activity = Instant::now();
    let mut last_stats = Instant::now();
    let mut rate_start = Instant::now();
    let mut event_count = 0u32;
    let mut stream_epoch = 0u64;
    let mut last_capture_sequence = None;
    let mut unique_capture_frames = 0u64;
    let outcome: Result<(), String> = async {
        loop {
            if pending.is_none() && in_flight.is_empty()
                && let Some(new) = requested.take() {
                    desktop.input(InputEvent::Release, settings.monitor).await?;
                    settings = new;
                    stream_epoch = stream_epoch.wrapping_add(1);
                    last_capture_sequence = None;
                    unique_capture_frames = 0;
                    let mut reply = serde_json::to_value(&settings).unwrap();
                    reply["type"] = "settings".into();
                    send_json(&tx, reply)?;
            }
            tokio::select! {
                _ = &mut writer => { return Ok(()); }
                _ = shutdown.changed() => { return Ok(()); }
                message = source.next() => {
                    let Some(Ok(message)) = message else { return Ok(()); };
                    if !state.auth.valid(&token) { return Err("세션이 만료되었거나 로그아웃되었습니다.".into()); }
                    last_activity = Instant::now();
                    if rate_start.elapsed() >= Duration::from_secs(1) { event_count = 0; rate_start = Instant::now(); }
                    event_count += 1;
                    if event_count > 500 { return Err("입력 전송 속도가 허용 범위를 초과했습니다.".into()); }
                    let text = match message {
                        Message::Text(text) => text,
                        Message::Close(_) => return Ok(()),
                        Message::Ping(_) | Message::Pong(_) => continue,
                        _ => return Err("클라이언트는 JSON 메시지만 전송할 수 있습니다.".into()),
                    };
                    if let Ok(control) = serde_json::from_str::<ControlMessage>(&text) {
                        match control {
                            ControlMessage::Ack => { in_flight.pop_front(); }
                            ControlMessage::Ping { sent } => { send_json(&tx, json!({"type":"pong", "sent":sent}))?; }
                            ControlMessage::Settings { monitor, fps, quality, max_width, codec } => {
                                let new = StreamSettings { monitor, fps, quality, max_width, codec };
                                new.validate(&monitors)?;
                                requested = Some(new);
                            }
                        }
                    } else {
                        let event: InputEvent = serde_json::from_str(&text).map_err(|_| "알 수 없거나 잘못된 입력 메시지입니다.")?;
                        event.validate()?;
                        // Never dispatch input during a monitor switch; the displayed coordinate space is changing.
                        if requested.as_ref().is_none_or(|next| next.monitor == settings.monitor) || matches!(event, InputEvent::Release) {
                            desktop.input(event, settings.monitor).await?;
                        }
                    }
                }
                result = async { match pending.as_mut() { Some(task) => task.await, None => std::future::pending().await } } => {
                    pending = None;
                    let frame = result.map_err(|_| "프레임 작업이 종료되었습니다.")??;
                    if frame.capture_sequence.is_none() || frame.capture_sequence != last_capture_sequence {
                        unique_capture_frames += 1;
                    }
                    last_capture_sequence = frame.capture_sequence;
                    if let Some(codec) = &frame.encoded.decoder_config { send_json(&tx, json!({"type":"decoder", "codec":codec}))?; }
                    let mut data = frame.encoded.data;
                    if frame.encoded.codec == Codec::H264 {
                        let mut envelope = Vec::with_capacity(data.len() + 5);
                        envelope.extend_from_slice(b"RWH1");
                        envelope.push(u8::from(frame.encoded.key));
                        envelope.append(&mut data);
                        data = envelope;
                    }
                    tx.try_send(Message::Binary(data.into())).map_err(|_| "프레임 전송 대기열이 가득 찼습니다.")?;
                    in_flight.push_back(Instant::now());
                    if last_stats.elapsed() >= Duration::from_secs(1) {
                        send_json(&tx, json!({"type":"stats", "stream_epoch":stream_epoch, "capture_ms":frame.capture_ms, "encode_ms":frame.encode_ms, "capture_backend":frame.capture_backend, "capture_sequence":frame.capture_sequence, "capture_age_ms":frame.capture_age_ms, "unique_capture_frames":unique_capture_frames, "in_flight":in_flight.len()}))?;
                        last_stats = Instant::now();
                    }
                }
                _ = interval.tick() => {
                    if !state.auth.valid(&token) { return Err("세션이 만료되었거나 로그아웃되었습니다.".into()); }
                    if last_activity.elapsed() > Duration::from_secs(30) || in_flight.front().is_some_and(|time| time.elapsed() > Duration::from_secs(10)) { return Err("클라이언트 응답 시간이 초과되었습니다.".into()); }
                }
                _ = tokio::time::sleep_until(next_frame.into()), if pending.is_none() && requested.is_none() && in_flight.len() < 2 => {
                    next_frame = Instant::now() + Duration::from_secs_f64(1.0 / settings.fps as f64);
                    pending = Some(tokio::spawn(worker::frame(desktop.clone(), encoder.clone(), settings.clone(), stream_epoch)));
                }
            }
        }
    }.await;
    // Stop native input first, even while a slow browser is still draining the network.
    desktop.stop().await;
    if let Some(task) = pending {
        task.abort();
    }
    if let Err(message) = outcome {
        tracing::info!(%message, "Remote session ended");
        let _ = send_json(
            &tx,
            json!({"type":"error", "message":message, "recoverable":false}),
        );
    }
    let code = if state.auth.valid(&token) { 1000 } else { 4401 };
    let _ = tx.try_send(Message::Close(Some(CloseFrame {
        code,
        reason: "Session ended".into(),
    })));
    drop(tx);
    if !writer.is_finished() {
        let _ = writer.await;
    }
}
