//! The session engine sees the same messages over WebSocket and WebTransport.
//! WebTransport uses one ordered, reliable bidi stream (kind:u8, length:u32be, payload).
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::Deserialize;
use std::{io, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

const MAX_INPUT: usize = 16 * 1024;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

pub struct Socket {
    sender: Sender,
    receiver: Receiver,
}

pub enum Sender {
    WebSocket(SplitSink<WebSocket, Message>),
    Http3 {
        stream: wtransport::SendStream,
        connection: Arc<wtransport::Connection>,
    },
}

pub enum Receiver {
    WebSocket(SplitStream<WebSocket>),
    Http3(Http3Receiver),
}

pub struct Http3Receiver {
    messages: mpsc::Receiver<Result<Message, String>>,
    task: JoinHandle<()>,
}

impl Drop for Http3Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Socket {
    pub fn websocket(socket: WebSocket) -> Self {
        let (sender, receiver) = socket.split();
        Self {
            sender: Sender::WebSocket(sender),
            receiver: Receiver::WebSocket(receiver),
        }
    }

    pub fn http3(
        connection: wtransport::Connection,
        send: wtransport::SendStream,
        mut recv: wtransport::RecvStream,
    ) -> Self {
        let (tx, messages) = mpsc::channel(16);
        // read_exact is not cancellation safe. Keep framing in its own bounded task,
        // so select! in the session loop cannot discard a partial frame header/body.
        let task = tokio::spawn(async move {
            loop {
                let result = read_message(&mut recv)
                    .await
                    .map_err(|error| error.to_string());
                let end = !matches!(&result, Ok(Message::Text(_)));
                if tx.send(result).await.is_err() || end {
                    break;
                }
            }
        });
        Self {
            sender: Sender::Http3 {
                stream: send,
                connection: Arc::new(connection),
            },
            receiver: Receiver::Http3(Http3Receiver { messages, task }),
        }
    }

    pub fn name(&self) -> &'static str {
        match &self.sender {
            Sender::WebSocket(_) => "websocket",
            Sender::Http3 { .. } => "http3",
        }
    }

    pub async fn send(&mut self, message: Message) -> Result<(), String> {
        self.sender.send(message).await
    }
    pub async fn close(&mut self) {
        let _ = self.sender.close().await;
    }
    pub fn split(self) -> (Sender, Receiver) {
        (self.sender, self.receiver)
    }
}

impl Sender {
    pub async fn send(&mut self, message: Message) -> Result<(), String> {
        match self {
            Self::WebSocket(sender) => sender
                .send(message)
                .await
                .map_err(|error| error.to_string()),
            Self::Http3 { stream, .. } => write_message(stream, message)
                .await
                .map_err(|error| error.to_string()),
        }
    }

    pub async fn close(&mut self) -> Result<(), String> {
        match self {
            Self::WebSocket(sender) => sender.close().await.map_err(|error| error.to_string()),
            Self::Http3 { stream, connection } => {
                let result = stream.finish().await.map_err(|error| error.to_string());
                connection.close(wtransport::VarInt::from_u32(0), b"Session ended");
                result
            }
        }
    }
}

impl Receiver {
    pub async fn next(&mut self) -> Option<Result<Message, String>> {
        match self {
            Self::WebSocket(receiver) => receiver
                .next()
                .await
                .map(|result| result.map_err(|error| error.to_string())),
            Self::Http3(receiver) => receiver.messages.recv().await,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Close {
    code: u16,
    reason: String,
}

async fn read_message(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Message> {
    let mut header = [0; 5];
    reader.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
    if !matches!(header[0], 0 | 2) || length > MAX_INPUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid client frame type or length",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    let text = String::from_utf8(payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if header[0] == 0 {
        return Ok(Message::Text(text.into()));
    }
    let close: Close = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if close.reason.len() > 123 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Close reason is too long",
        ));
    }
    Ok(Message::Close(Some(CloseFrame {
        code: close.code,
        reason: close.reason.into(),
    })))
}

async fn write_message(writer: &mut (impl AsyncWrite + Unpin), message: Message) -> io::Result<()> {
    let close_payload;
    let (kind, payload) = match &message {
        Message::Text(text) => (0, text.as_bytes()),
        Message::Binary(bytes) => (1, bytes.as_ref()),
        Message::Close(frame) => {
            let (code, reason) = frame
                .as_ref()
                .map(|frame| (frame.code, frame.reason.to_string()))
                .unwrap_or((1000, String::new()));
            close_payload =
                serde_json::to_vec(&serde_json::json!({"code":code, "reason":reason})).unwrap();
            (2, close_payload.as_slice())
        }
        Message::Ping(_) | Message::Pong(_) => return Ok(()),
    };
    if payload.len() > MAX_OUTPUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Server frame is too large",
        ));
    }
    let mut header = [0; 5];
    header[0] = kind;
    header[1..].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fragmented_and_coalesced_messages_preserve_boundaries() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let task = tokio::spawn(async move {
            for byte in b"\0\0\0\0\x02{}\0\0\0\0\x02[]" {
                writer.write_all(&[*byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        assert_eq!(
            read_message(&mut reader).await.unwrap(),
            Message::Text("{}".into())
        );
        assert_eq!(
            read_message(&mut reader).await.unwrap(),
            Message::Text("[]".into())
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_oversized_binary_and_invalid_utf8_before_dispatch() {
        for bytes in [
            vec![0, 0, 0, 64, 1],
            vec![1, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 1, 255],
        ] {
            assert!(read_message(&mut bytes.as_slice()).await.is_err());
        }
    }

    #[tokio::test]
    async fn close_frame_roundtrips() {
        let mut bytes = Vec::new();
        let close = Message::Close(Some(CloseFrame {
            code: 4401,
            reason: "Expired".into(),
        }));
        write_message(&mut bytes, close.clone()).await.unwrap();
        assert_eq!(read_message(&mut bytes.as_slice()).await.unwrap(), close);
    }
}
