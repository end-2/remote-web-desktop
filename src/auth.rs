use axum::http::HeaderMap;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

pub const COOKIE: &str = "rwd_session";
pub const SESSION_SECONDS: u64 = 8 * 60 * 60;

pub struct Auth {
    password_hash: [u8; 32],
    sessions: Mutex<HashMap<String, Instant>>,
    attempts: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Auth {
    pub fn new(password: &str) -> Self {
        Self {
            password_hash: Sha256::digest(password.as_bytes()).into(),
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Limit attempts before password verification; proxy headers are never trusted.
    pub fn allow_attempt(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().unwrap();
        attempts.retain(|_, times| {
            times.retain(|time| now.duration_since(*time) < Duration::from_secs(60));
            !times.is_empty()
        });
        if attempts.len() >= 4096 && !attempts.contains_key(&ip) {
            return false;
        }
        let times = attempts.entry(ip).or_default();
        if times.len() >= 10 {
            return false;
        }
        times.push(now);
        true
    }

    pub fn login(&self, password: &str) -> Option<String> {
        let supplied: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        if !bool::from(self.password_hash.ct_eq(&supplied)) {
            return None;
        }
        let token = random_token();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, expires| *expires > Instant::now());
        if sessions.len() >= 16 {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, time)| **time)
                .map(|(token, _)| token.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            token.clone(),
            Instant::now() + Duration::from_secs(SESSION_SECONDS),
        );
        Some(token)
    }

    pub fn valid(&self, token: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(token)
            .is_some_and(|expires| *expires > Instant::now())
    }

    pub fn revoke(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }
}

pub fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == COOKIE && value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| value.to_string())
        })
}

pub fn same_origin(headers: &HeaderMap, public_origin: Option<&str>) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if let Some(expected) = public_origin {
        return origin == expected;
    }
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin == format!("http://{host}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sessions_are_random_and_revocable() {
        let auth = Auth::new("correct password");
        assert!(auth.login("incorrect password").is_none());
        let a = auth.login("correct password").unwrap();
        let b = auth.login("correct password").unwrap();
        assert_ne!(a, b);
        assert!(auth.valid(&a));
        auth.revoke(&a);
        assert!(!auth.valid(&a));
        assert!(auth.valid(&b));
    }

    #[test]
    fn rejects_cross_origin_and_null_origins() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost:8080".parse().unwrap());
        assert!(!same_origin(&headers, None));
        headers.insert("origin", "null".parse().unwrap());
        assert!(!same_origin(&headers, None));
        headers.insert("origin", "http://localhost:8080".parse().unwrap());
        assert!(same_origin(&headers, None));
        headers.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!same_origin(&headers, None));
        headers.insert("origin", "https://remote.example".parse().unwrap());
        assert!(same_origin(&headers, Some("https://remote.example")));
    }

    #[test]
    fn limits_brute_force_attempts() {
        let auth = Auth::new("password");
        let ip = "127.0.0.1".parse().unwrap();
        for _ in 0..10 {
            assert!(auth.allow_attempt(ip));
        }
        assert!(!auth.allow_attempt(ip));
        assert!(auth.allow_attempt("127.0.0.2".parse().unwrap()));
    }
}
