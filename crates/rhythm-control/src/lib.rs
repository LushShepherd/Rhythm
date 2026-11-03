//! Versioned, local-only desktop control. Machine clients never open the live database.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(any(windows, test))]
use sha2::{Digest, Sha256};
#[cfg(any(windows, test))]
use std::collections::{HashMap, VecDeque};
use std::{io, path::Path};

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::{ServerGuard, send, start_server, start_server_with_completion};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 1_048_576;
pub const MAX_RESPONSE_BYTES: usize = 33_554_432;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
}

impl ControlRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("Unsupported protocol version");
        }
        if self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err("request_id must contain 1 to 128 bytes");
        }
        match (self.operation.as_str(), &self.request) {
            (
                "snapshot" | "status" | "journal" | "drafts" | "settings" | "routines"
                | "invitations" | "quit",
                None,
            )
            | ("execute", Some(_)) => Ok(()),
            _ => Err(
                "Use a supported read operation or quit without request, or execute with a DesktopRequest object",
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ControlError {}
impl ControlError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}
impl ControlResponse {
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    pub fn failure(
        id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: id.into(),
            ok: false,
            result: None,
            error: Some(ControlError::new(code, message)),
        }
    }
}

/// Bounded replay protection. Even when a response is evicted, its ID cannot execute
/// again during this server lifetime. No journal text enters the fingerprint ledger.
#[derive(Default)]
#[cfg(any(windows, test))]
struct ReplayCache {
    seen: HashMap<String, [u8; 32]>,
    responses: VecDeque<(String, ControlResponse, usize)>,
    bytes: usize,
}
#[cfg(any(windows, test))]
impl ReplayCache {
    fn handle(
        &mut self,
        request: ControlRequest,
        handler: &impl Fn(ControlRequest) -> ControlResponse,
    ) -> ControlResponse {
        if let Err(message) = request.validate() {
            return ControlResponse::failure(request.request_id, "invalid_request", message);
        }
        if !matches!(request.operation.as_str(), "execute" | "quit") {
            return handler(request);
        }
        let fingerprint: [u8; 32] = Sha256::digest(serde_json::to_vec(&request).unwrap()).into();
        if let Some(previous) = self.seen.get(&request.request_id) {
            if previous != &fingerprint {
                return ControlResponse::failure(
                    request.request_id,
                    "request_id_conflict",
                    "This request ID was already used with different content.",
                );
            }
            return self.responses.iter().find(|(id, _, _)| id == &request.request_id).map(|(_, response, _)| response.clone()).unwrap_or_else(|| ControlResponse::failure(request.request_id, "response_expired", "This request was already processed; its cached response expired. Inspect current state. It was not executed again."));
        }
        if self.seen.len() >= 8192 {
            return ControlResponse::failure(
                request.request_id,
                "control_capacity",
                "The control replay ledger is full. Restart Rhythm before sending new changes.",
            );
        }
        self.seen.insert(request.request_id.clone(), fingerprint);
        let response = handler(request);
        let size = serde_json::to_vec(&response).unwrap().len();
        if size <= 64 * 1024 * 1024 {
            while self.responses.len() >= 256 || self.bytes + size > 64 * 1024 * 1024 {
                if let Some((_, _, bytes)) = self.responses.pop_front() {
                    self.bytes -= bytes;
                } else {
                    break;
                }
            }
            self.bytes += size;
            self.responses
                .push_back((response.request_id.clone(), response.clone(), size));
        }
        response
    }
}

#[cfg(not(windows))]
pub struct ServerGuard;
#[cfg(not(windows))]
pub fn start_server(
    directory: &Path,
    handler: impl Fn(ControlRequest) -> ControlResponse + Send + 'static,
) -> io::Result<ServerGuard> {
    start_server_with_completion(directory, handler, |_| {})
}
#[cfg(not(windows))]
pub fn start_server_with_completion(
    _directory: &Path,
    _handler: impl Fn(ControlRequest) -> ControlResponse + Send + 'static,
    _completion: impl Fn(&ControlResponse) + Send + 'static,
) -> io::Result<ServerGuard> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Desktop control currently supports Windows only",
    ))
}
#[cfg(not(windows))]
pub fn send(_directory: &Path, _request: &ControlRequest) -> Result<ControlResponse, ControlError> {
    Err(ControlError::new(
        "unsupported_platform",
        "Desktop control currently supports Windows only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn request(id: &str) -> ControlRequest {
        ControlRequest {
            protocol_version: 1,
            request_id: id.into(),
            operation: "execute".into(),
            request: Some(serde_json::json!({"op":"pause"})),
        }
    }
    #[test]
    fn retry_returns_same_result_without_repeating_changes() {
        let calls = Cell::new(0);
        let handler = |r: ControlRequest| {
            calls.set(calls.get() + 1);
            ControlResponse::success(r.request_id, Value::Null)
        };
        let mut cache = ReplayCache::default();
        let first = cache.handle(request("one"), &handler);
        assert_eq!(cache.handle(request("one"), &handler), first);
        let mut conflicting = request("one");
        conflicting.request = Some(serde_json::json!({"op":"resume"}));
        assert_eq!(
            cache.handle(conflicting, &handler).error.unwrap().code,
            "request_id_conflict"
        );
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn evicted_response_does_not_repeat_a_mutation() {
        let mut cache = ReplayCache::default();
        let handler = |r: ControlRequest| ControlResponse::success(r.request_id, Value::Null);
        for id in 0..257 {
            cache.handle(request(&id.to_string()), &handler);
        }
        assert_eq!(
            cache.handle(request("0"), &handler).error.unwrap().code,
            "response_expired"
        );
    }
    #[test]
    fn invalid_protocol_and_unknown_fields_are_rejected() {
        let mut r = request("a");
        r.protocol_version = 2;
        assert!(r.validate().is_err());
        assert!(serde_json::from_value::<ControlRequest>(serde_json::json!({"protocol_version":1,"request_id":"a","operation":"snapshot","surprise":true})).is_err());
    }

    #[test]
    fn quit_requires_no_payload_and_uses_mutation_replay_protection() {
        let quit = ControlRequest {
            protocol_version: 1,
            request_id: "quit-once".into(),
            operation: "quit".into(),
            request: None,
        };
        assert!(quit.validate().is_ok());
        let mut invalid = quit.clone();
        invalid.request = Some(serde_json::json!({"force":true}));
        assert!(invalid.validate().is_err());
        let calls = Cell::new(0);
        let handler = |request: ControlRequest| {
            calls.set(calls.get() + 1);
            ControlResponse::success(request.request_id, serde_json::json!({"closing":true}))
        };
        let mut cache = ReplayCache::default();
        let first = cache.handle(quit.clone(), &handler);
        assert_eq!(cache.handle(quit, &handler), first);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            cache
                .handle(request("quit-once"), &handler)
                .error
                .unwrap()
                .code,
            "request_id_conflict"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn quit_cannot_bypass_a_full_mutation_ledger() {
        let mut cache = ReplayCache::default();
        for id in 0..8192 {
            cache.seen.insert(id.to_string(), [0; 32]);
        }
        let quit = ControlRequest {
            protocol_version: 1,
            request_id: "quit-over-capacity".into(),
            operation: "quit".into(),
            request: None,
        };
        let response = cache.handle(quit, &|_| panic!("Full ledger must not execute quit"));
        assert_eq!(response.error.unwrap().code, "control_capacity");
    }
}
