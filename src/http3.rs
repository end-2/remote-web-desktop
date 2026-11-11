//! HTTP/3 WebTransport listener with short-lived, origin-bound session tickets.
use axum::http::{HeaderMap, uri::Authority};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use tokio::{sync::Semaphore, task::JoinSet};
use wtransport::{Endpoint, Identity, ServerConfig, endpoint::endpoint_side::Server};

use crate::{
    auth,
    server::{self, AppState},
    transport,
};

const TICKET_LIFETIME: Duration = Duration::from_secs(30);
// Certificates are valid for 14 days. Pre-advertise the next certificate and
// rotate every 6 days, leaving a 2-day scheduling margin even for the next cert.
const ROTATION_INTERVAL: Duration = Duration::from_secs(6 * 24 * 60 * 60);

struct Certificates {
    current_hash: [u8; 32],
    next: Identity,
    next_created: SystemTime,
    last_rotated: SystemTime,
}
struct Ticket {
    token: String,
    origin: String,
    authority: String,
    expires: Instant,
}

pub struct Http3 {
    endpoint: Endpoint<Server>,
    address: SocketAddr,
    certificates: Mutex<Certificates>,
    tickets: Mutex<HashMap<String, Ticket>>,
}

fn identity() -> Result<Identity, String> {
    // Certificate hashes authenticate this identity; no OS/browser trust store
    // modification is needed and the private key never leaves this process.
    Identity::self_signed(["localhost", "127.0.0.1", "::1"]).map_err(|error| error.to_string())
}

fn config(address: SocketAddr, identity: Identity) -> ServerConfig {
    ServerConfig::builder()
        .with_bind_address(address)
        .with_identity(identity)
        .max_idle_timeout(Some(Duration::from_secs(30)))
        .expect("valid QUIC idle timeout")
        .keep_alive_interval(Some(Duration::from_secs(5)))
        .build()
}

fn hash(identity: &Identity) -> [u8; 32] {
    *identity.certificate_chain().as_slice()[0].hash().as_ref()
}

/// Never interpolate an unvalidated Host header into a CSP or connection URL.
fn hostname(authority: &str) -> Option<String> {
    if authority.contains('@') {
        return None;
    }
    let authority: Authority = authority.parse().ok()?;
    let host = authority.host();
    let unbracketed = host.trim_start_matches('[').trim_end_matches(']');
    if unbracketed.parse::<std::net::IpAddr>().is_err()
        && (host.is_empty()
            || !host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-')))
    {
        return None;
    }
    Some(host.to_owned())
}

pub fn origin(headers: &HeaderMap, public_origin: Option<&str>, port: u16) -> Option<String> {
    let host = if let Some(origin) = public_origin {
        let uri: axum::http::Uri = origin.parse().ok()?;
        hostname(uri.authority()?.as_str())?
    } else {
        hostname(headers.get("host")?.to_str().ok()?)?
    };
    Some(if port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{port}")
    })
}

impl Http3 {
    pub fn bind(address: SocketAddr) -> Result<Arc<Self>, String> {
        let current = identity()?;
        let current_hash = hash(&current);
        let endpoint =
            Endpoint::server(config(address, current)).map_err(|error| error.to_string())?;
        let address = endpoint.local_addr().map_err(|error| error.to_string())?;
        Ok(Arc::new(Self {
            endpoint,
            address,
            certificates: Mutex::new(Certificates {
                current_hash,
                next: identity()?,
                next_created: SystemTime::now(),
                last_rotated: SystemTime::now(),
            }),
            tickets: Mutex::new(HashMap::new()),
        }))
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn bootstrap(
        &self,
        token: String,
        request_origin: String,
        endpoint_origin: String,
        auth: &auth::Auth,
    ) -> Option<Value> {
        // Wall-clock validation also handles a machine waking from a long sleep,
        // before advertising a hash whose certificate has already expired.
        self.rotate_if_due().ok()?;
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, ticket| ticket.expires > now && auth.valid(&ticket.token));
        // Bound memory and pending tickets per login without disrupting sessions.
        if tickets
            .values()
            .filter(|ticket| ticket.token == token)
            .count()
            >= 4
        {
            let oldest = tickets
                .iter()
                .filter(|(_, ticket)| ticket.token == token)
                .min_by_key(|(_, ticket)| ticket.expires)
                .map(|(key, _)| key.clone())
                .unwrap();
            tickets.remove(&oldest);
        }
        if tickets.len() >= 64 {
            return None;
        }
        let ticket = auth::random_token();
        let authority = endpoint_origin.strip_prefix("https://")?.to_owned();
        tickets.insert(
            ticket.clone(),
            Ticket {
                token,
                origin: request_origin,
                authority,
                expires: now + TICKET_LIFETIME,
            },
        );
        let certificates = self.certificates.lock().unwrap();
        Some(json!({
            "mode":"webtransport", "protocol":"rwd-stream-v1",
            "url":format!("{endpoint_origin}/remote?ticket={ticket}"),
            "certificate_hash":certificates.current_hash,
            "certificate_hashes":[certificates.current_hash, hash(&certificates.next)],
            "expires_in":TICKET_LIFETIME.as_secs(),
        }))
    }

    fn consume(
        &self,
        path: &str,
        origin: Option<&str>,
        authority: &str,
        auth: &auth::Auth,
    ) -> Option<String> {
        let key = path.strip_prefix("/remote?ticket=")?;
        if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let mut tickets = self.tickets.lock().unwrap();
        let ticket = tickets.get(key)?;
        if ticket.expires <= Instant::now() || !auth.valid(&ticket.token) {
            tickets.remove(key);
            return None;
        }
        if origin != Some(ticket.origin.as_str()) || authority != ticket.authority {
            return None;
        }
        Some(tickets.remove(key)?.token)
    }

    fn rotate(&self) -> Result<(), String> {
        let next = identity()?;
        let mut certificates = self.certificates.lock().unwrap();
        let current = if certificates
            .next_created
            .elapsed()
            .is_ok_and(|age| age <= Duration::from_secs(7 * 24 * 60 * 60))
        {
            certificates.next.clone_identity()
        } else {
            // Long suspension or a clock change can invalidate the staged cert.
            // Old pending tickets may fall back once; new bootstraps get fresh hashes.
            identity()?
        };
        // Keep both advertised hashes consistent with the endpoint across rotation.
        self.endpoint
            .reload_config(config(self.address, current.clone_identity()), false)
            .map_err(|error| error.to_string())?;
        certificates.current_hash = hash(&current);
        certificates.next = next;
        certificates.next_created = SystemTime::now();
        certificates.last_rotated = SystemTime::now();
        Ok(())
    }

    fn rotate_if_due(&self) -> Result<(), String> {
        let due = self
            .certificates
            .lock()
            .unwrap()
            .last_rotated
            .elapsed()
            .map_or(true, |age| age >= ROTATION_INTERVAL);
        if due {
            self.rotate()?;
        }
        Ok(())
    }

    pub async fn serve(self: Arc<Self>, state: Arc<AppState>) {
        let mut shutdown = state.shutdown.clone();
        let permits = Arc::new(Semaphore::new(32));
        let mut tasks = JoinSet::new();
        let mut rotation = tokio::time::interval(Duration::from_secs(60 * 60));
        rotation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(address = %self.address, "HTTP/3 WebTransport listener ready");
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = rotation.tick() => { if let Err(error) = self.rotate_if_due() { tracing::error!(%error, "HTTP/3 certificate rotation failed"); } },
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                incoming = self.endpoint.accept() => {
                    let Ok(permit) = permits.clone().try_acquire_owned() else { incoming.refuse(); continue; };
                    let server = self.clone();
                    let state = state.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let Ok(Ok(request)) = tokio::time::timeout(Duration::from_secs(5), async { incoming.await }).await else { return; };
                        let Some(token) = server.consume(request.path(), request.origin(), request.authority(), &state.auth) else {
                            let _ = tokio::time::timeout(Duration::from_secs(2), request.forbidden()).await;
                            return;
                        };
                        let Ok(Ok(connection)) = tokio::time::timeout(Duration::from_secs(5), request.accept()).await else { return; };
                        let Ok(Ok((send, recv))) = tokio::time::timeout(Duration::from_secs(5), connection.accept_bi()).await else {
                            connection.close(wtransport::VarInt::from_u32(1), b"Stream timeout");
                            return;
                        };
                        // Recheck after handshake so logout can never race input startup.
                        let token = state.auth.valid(&token).then_some(token);
                        server::connection(transport::Socket::http3(connection, send, recv), state, token).await;
                    });
                }
            }
        }
        self.endpoint
            .close(wtransport::VarInt::from_u32(0), b"Server shutdown");
        // Session tasks observe the shared shutdown watch and release held input.
        while tasks.join_next().await.is_some() {}
        let _ = tokio::time::timeout(Duration::from_secs(2), self.endpoint.wait_idle()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_validation_rejects_csp_and_url_injection() {
        let mut headers = HeaderMap::new();
        for host in [
            "a;https://evil",
            "user@evil",
            "evil/path",
            "'unsafe-inline'",
            "*.evil",
        ] {
            headers.insert("host", host.parse().unwrap());
            assert!(origin(&headers, None, 8443).is_none(), "{host}");
        }
        headers.insert("host", "[::1]:8080".parse().unwrap());
        assert_eq!(origin(&headers, None, 8443).unwrap(), "https://[::1]:8443");
    }

    #[tokio::test]
    async fn tickets_bind_origin_authority_session_and_cannot_be_replayed() {
        let server = Http3::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let auth = auth::Auth::new("password");
        let token = auth.login("password").unwrap();
        let endpoint = "https://127.0.0.1:8443".to_owned();
        let response = server
            .bootstrap(
                token.clone(),
                "http://localhost:8080".into(),
                endpoint.clone(),
                &auth,
            )
            .unwrap();
        let path = response["url"]
            .as_str()
            .unwrap()
            .strip_prefix(&endpoint)
            .unwrap();
        assert!(
            server
                .consume(path, Some("http://evil"), "127.0.0.1:8443", &auth)
                .is_none()
        );
        assert!(
            server
                .consume(path, Some("http://localhost:8080"), "localhost:8443", &auth)
                .is_none()
        );
        assert_eq!(
            server.consume(path, Some("http://localhost:8080"), "127.0.0.1:8443", &auth),
            Some(token.clone())
        );
        assert!(
            server
                .consume(path, Some("http://localhost:8080"), "127.0.0.1:8443", &auth)
                .is_none()
        );
        let response = server
            .bootstrap(
                token.clone(),
                "http://localhost:8080".into(),
                endpoint.clone(),
                &auth,
            )
            .unwrap();
        auth.revoke(&token);
        let path = response["url"]
            .as_str()
            .unwrap()
            .strip_prefix(&endpoint)
            .unwrap();
        assert!(
            server
                .consume(path, Some("http://localhost:8080"), "127.0.0.1:8443", &auth)
                .is_none()
        );
    }

    #[tokio::test]
    async fn expired_tickets_are_rejected_and_pending_tickets_are_bounded() {
        let server = Http3::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let auth = auth::Auth::new("password");
        let token = auth.login("password").unwrap();
        for _ in 0..10 {
            server
                .bootstrap(
                    token.clone(),
                    "http://localhost".into(),
                    "https://localhost:8443".into(),
                    &auth,
                )
                .unwrap();
        }
        assert_eq!(server.tickets.lock().unwrap().len(), 4);
        let key = server
            .tickets
            .lock()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        server
            .tickets
            .lock()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(
            server
                .consume(
                    &format!("/remote?ticket={key}"),
                    Some("http://localhost"),
                    "localhost:8443",
                    &auth
                )
                .is_none()
        );
    }

    #[tokio::test]
    async fn rotation_promotes_the_pre_advertised_certificate() {
        let server = Http3::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let before = hash(&server.certificates.lock().unwrap().next);
        server.rotate().unwrap();
        let certificates = server.certificates.lock().unwrap();
        assert_eq!(certificates.current_hash, before);
        assert_ne!(certificates.current_hash, hash(&certificates.next));
    }

    #[tokio::test]
    async fn long_suspension_replaces_stale_staged_certificate() {
        let server = Http3::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let before = {
            let mut certificates = server.certificates.lock().unwrap();
            certificates.next_created = SystemTime::now() - Duration::from_secs(15 * 24 * 60 * 60);
            certificates.last_rotated = certificates.next_created;
            hash(&certificates.next)
        };
        server.rotate_if_due().unwrap();
        assert_ne!(server.certificates.lock().unwrap().current_hash, before);
    }
}
