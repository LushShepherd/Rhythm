use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{DRIVE_SCOPE, Error, Result};

pub(crate) const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct OAuthClientConfig {
    pub client_id: String,
    /// Desktop apps are public clients; this value is not a confidential secret.
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for OAuthClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthClientConfig([REDACTED])")
    }
}

impl OAuthClientConfig {
    pub fn from_google_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 {
            return Err(Error::Configuration);
        }
        #[derive(Deserialize)]
        struct Download {
            installed: Installed,
        }
        #[derive(Deserialize)]
        struct Installed {
            client_id: String,
            client_secret: Option<String>,
        }
        let source: Download = serde_json::from_slice(bytes).map_err(|_| Error::Configuration)?;
        let config = Self {
            client_id: source.installed.client_id,
            client_secret: source.installed.client_secret,
        };
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if !self.client_id.ends_with(".apps.googleusercontent.com")
            || self.client_id.len() > 512
            || self.client_id.len() < 30
            || !self
                .client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
            || self
                .client_secret
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 2048 || s.chars().any(char::is_control))
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}

/// Persist only in OS-protected storage; never return these to the webview.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix_s: u64,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens([REDACTED])")
    }
}

impl Tokens {
    pub(crate) fn valid(&self) -> bool {
        !self.access_token.is_empty() && self.expires_at_unix_s > now().saturating_add(60)
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn http_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Rhythm/0.1 GoogleDriveSync")
        .build()
        .map_err(|_| Error::Retryable)
}

fn random_string() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub struct AuthorizationAttempt {
    listener: TcpListener,
    config: OAuthClientConfig,
    state: String,
    verifier: String,
    redirect_uri: String,
    url: String,
}

impl Drop for AuthorizationAttempt {
    fn drop(&mut self) {
        self.state.zeroize();
        self.verifier.zeroize();
        self.url.zeroize();
    }
}

impl AuthorizationAttempt {
    pub fn begin(config: &OAuthClientConfig) -> Result<Self> {
        config.validate()?;
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|_| Error::Loopback)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| Error::Loopback)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr().map_err(|_| Error::Loopback)?.port()
        );
        let state = random_string();
        let verifier = random_string();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse(AUTHORIZE_URL).map_err(|_| Error::Configuration)?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", DRIVE_SCOPE),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent select_account"),
        ]);
        Ok(Self {
            listener,
            config: config.clone(),
            state,
            verifier,
            redirect_uri,
            url: url.into(),
        })
    }

    /// Open this URL in the system browser, never in an embedded webview.
    pub fn authorization_url(&self) -> &str {
        &self.url
    }

    pub fn finish(self, timeout: Duration) -> Result<Tokens> {
        self.finish_with_cancel(timeout, || false)
    }

    /// Run on the dedicated sync worker. Cancellation is checked every 50ms
    /// while waiting; a stalled local connection is bounded to two seconds.
    pub fn finish_with_cancel(
        self,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<Tokens> {
        let deadline = Instant::now() + timeout.min(Duration::from_secs(300));
        loop {
            if cancelled() {
                return Err(Error::AuthorizationDenied);
            }
            if Instant::now() >= deadline {
                return Err(Error::AuthorizationTimeout);
            }
            match self.listener.accept() {
                Ok((mut stream, remote)) => {
                    if !remote.ip().is_loopback() {
                        continue;
                    }
                    let parsed = read_callback(&mut stream, &self.redirect_uri, &self.state);
                    match parsed {
                        Ok(Callback::Code(code)) => {
                            respond(
                                &mut stream,
                                "200 OK",
                                "Sign-in received. You can close this window and return to Rhythm.",
                            );
                            if cancelled() {
                                return Err(Error::AuthorizationDenied);
                            }
                            let mut form = vec![
                                ("grant_type", "authorization_code".to_string()),
                                ("code", code),
                                ("client_id", self.config.client_id.clone()),
                                ("redirect_uri", self.redirect_uri.clone()),
                                ("code_verifier", self.verifier.clone()),
                            ];
                            if let Some(secret) = &self.config.client_secret {
                                form.push(("client_secret", secret.clone()));
                            }
                            return exchange(&http_client()?, TOKEN_URL, &form, None);
                        }
                        Ok(Callback::Denied) => {
                            respond(
                                &mut stream,
                                "200 OK",
                                "Sign-in was cancelled. You can return to Rhythm.",
                            );
                            return Err(Error::AuthorizationDenied);
                        }
                        Err(_) => respond(
                            &mut stream,
                            "400 Bad Request",
                            "This sign-in response is invalid. Return to the Google sign-in window.",
                        ),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(_) => return Err(Error::Loopback),
            }
        }
    }
}

enum Callback {
    Code(String),
    Denied,
}

fn read_callback(
    stream: &mut TcpStream,
    redirect_uri: &str,
    expected_state: &str,
) -> Result<Callback> {
    // Accepted sockets inherit the listener's nonblocking mode on some systems
    // (notably BSD/macOS). The header reader relies on bounded blocking reads,
    // so configure that mode explicitly rather than treating WouldBlock as a
    // malformed request and closing a browser's partially written callback.
    stream.set_nonblocking(false).map_err(|_| Error::Loopback)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| Error::Loopback)?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    while bytes.len() <= 16 * 1024 {
        let count = stream.read(&mut chunk).map_err(|_| Error::Protocol)?;
        if count == 0 {
            return Err(Error::Protocol);
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    if bytes.len() > 16 * 1024 {
        return Err(Error::Protocol);
    }
    let request = std::str::from_utf8(&bytes).map_err(|_| Error::Protocol)?;
    parse_callback(
        request.lines().next().ok_or(Error::Protocol)?,
        redirect_uri,
        expected_state,
    )
}

fn parse_callback(first_line: &str, redirect_uri: &str, expected_state: &str) -> Result<Callback> {
    let mut fields = first_line.split_whitespace();
    if fields.next() != Some("GET") {
        return Err(Error::Protocol);
    }
    let path = fields.next().ok_or(Error::Protocol)?;
    if !path.starts_with("/callback?")
        || fields.next() != Some("HTTP/1.1")
        || fields.next().is_some()
    {
        return Err(Error::Protocol);
    }
    let base = Url::parse(redirect_uri).map_err(|_| Error::Protocol)?;
    let url = base.join(path).map_err(|_| Error::Protocol)?;
    let pairs: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = pairs.iter().filter(|(key, _)| key == "state").collect();
    if states.len() != 1 || states[0].1 != expected_state {
        return Err(Error::Protocol);
    }
    let codes: Vec<_> = pairs.iter().filter(|(key, _)| key == "code").collect();
    let errors: Vec<_> = pairs.iter().filter(|(key, _)| key == "error").collect();
    if !errors.is_empty() {
        return if errors.len() == 1 && codes.is_empty() {
            Ok(Callback::Denied)
        } else {
            Err(Error::Protocol)
        };
    }
    if codes.len() != 1 || codes[0].1.is_empty() || codes[0].1.len() > 8192 {
        return Err(Error::Protocol);
    }
    Ok(Callback::Code(codes[0].1.to_string()))
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

pub(crate) fn refresh(
    client: &Client,
    config: &OAuthClientConfig,
    tokens: &Tokens,
    token_url: &str,
) -> Result<Tokens> {
    if tokens.refresh_token.is_empty() || tokens.refresh_token.len() > 16 * 1024 {
        return Err(Error::ReconnectRequired);
    }
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", tokens.refresh_token.clone()),
        ("client_id", config.client_id.clone()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    exchange(client, token_url, &form, Some(&tokens.refresh_token))
}

fn exchange(
    client: &Client,
    endpoint: &str,
    form: &[(&str, String)],
    previous_refresh: Option<&str>,
) -> Result<Tokens> {
    let response = client
        .post(endpoint)
        .form(form)
        .send()
        .map_err(|_| Error::Retryable)?;
    let status = response.status();
    let bytes = read_bounded(response, 64 * 1024)?;
    if !status.is_success() {
        let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        if status.as_u16() == 401 || error["error"] == "invalid_grant" {
            return Err(Error::ReconnectRequired);
        }
        if error["error"] == "invalid_client" || error["error"] == "unauthorized_client" {
            return Err(Error::Configuration);
        }
        return Err(
            if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
                Error::Retryable
            } else {
                Error::Remote(status.as_u16())
            },
        );
    }
    #[derive(Deserialize)]
    struct Response {
        access_token: String,
        token_type: String,
        expires_in: u64,
        refresh_token: Option<String>,
        scope: Option<String>,
    }
    let value: Response = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
    if !value.token_type.eq_ignore_ascii_case("bearer")
        || value.access_token.is_empty()
        || value.access_token.len() > 16 * 1024
        || value.expires_in == 0
        || value.expires_in > 86_400
        || value
            .scope
            .as_ref()
            .is_some_and(|s| !s.split_whitespace().any(|p| p == DRIVE_SCOPE))
    {
        return Err(Error::Protocol);
    }
    let refresh_token = value
        .refresh_token
        .or_else(|| previous_refresh.map(str::to_owned))
        .ok_or(Error::ReconnectRequired)?;
    if refresh_token.is_empty() || refresh_token.len() > 16 * 1024 {
        return Err(Error::Protocol);
    }
    Ok(Tokens {
        access_token: value.access_token,
        refresh_token,
        expires_at_unix_s: now().saturating_add(value.expires_in),
    })
}

pub(crate) fn read_bounded(
    mut response: reqwest::blocking::Response,
    limit: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(Error::ObjectTooLarge);
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Retryable)?;
    if bytes.len() > limit {
        return Err(Error::ObjectTooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuthClientConfig {
        OAuthClientConfig {
            client_id: "123456789-example.apps.googleusercontent.com".into(),
            client_secret: Some("public-installed-client-secret".into()),
        }
    }

    #[test]
    fn authorization_is_pkce_scoped_loopback_and_not_embedded() {
        let attempt = AuthorizationAttempt::begin(&config()).unwrap();
        let url = Url::parse(attempt.authorization_url()).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(pairs["scope"], DRIVE_SCOPE);
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(
            pairs["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(attempt.verifier.as_bytes()))
        );
        assert!(pairs["redirect_uri"].starts_with("http://127.0.0.1:"));
        assert!(!attempt.authorization_url().contains("client_secret"));
        assert!(!attempt.authorization_url().contains(&attempt.verifier));
    }

    #[test]
    fn callback_requires_exact_state_and_single_code() {
        let redirect = "http://127.0.0.1:1234/callback";
        assert!(matches!(
            parse_callback(
                "GET /callback?state=expected&code=a HTTP/1.1",
                redirect,
                "expected"
            ),
            Ok(Callback::Code(_))
        ));
        for request in [
            "GET /callback?state=other&code=a HTTP/1.1",
            "GET /callback?state=expected&state=expected&code=a HTTP/1.1",
            "GET /callback?state=expected&code=a&code=b HTTP/1.1",
            "GET /callback?state=expected&code=a&error=denied HTTP/1.1",
            "POST /callback?state=expected&code=a HTTP/1.1",
            "GET //evil.test/callback?state=expected&code=a HTTP/1.1",
        ] {
            assert!(parse_callback(request, redirect, "expected").is_err());
        }
        assert!(matches!(
            parse_callback(
                "GET /callback?state=expected&error=access_denied HTTP/1.1",
                redirect,
                "expected"
            ),
            Ok(Callback::Denied)
        ));
    }

    #[test]
    fn downloaded_config_must_be_desktop_and_does_not_control_endpoints() {
        assert!(
            OAuthClientConfig::from_google_json(
                br#"{"web":{"client_id":"123456789-example.apps.googleusercontent.com"}}"#
            )
            .is_err()
        );
        let parsed = OAuthClientConfig::from_google_json(br#"{"installed":{"client_id":"123456789-example.apps.googleusercontent.com","token_uri":"http://untrusted.test/token"}}"#).unwrap();
        assert!(parsed.client_secret.is_none());
        assert!(
            AuthorizationAttempt::begin(&parsed)
                .unwrap()
                .authorization_url()
                .starts_with(AUTHORIZE_URL)
        );
    }

    #[test]
    fn invalid_callback_does_not_consume_attempt_and_cancellation_is_bounded() {
        let attempt = AuthorizationAttempt::begin(&config()).unwrap();
        let address = attempt.listener.local_addr().unwrap();
        let state = attempt.state.clone();
        let handle = thread::spawn(move || attempt.finish(Duration::from_secs(5)));
        let send = |target: &str| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write!(stream, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        assert!(send("/callback?state=wrong&code=untrusted").starts_with("HTTP/1.1 400"));
        assert!(
            send(&format!("/callback?state={state}&error=access_denied"))
                .starts_with("HTTP/1.1 200")
        );
        assert!(matches!(
            handle.join().unwrap(),
            Err(Error::AuthorizationDenied)
        ));
        let attempt = AuthorizationAttempt::begin(&config()).unwrap();
        let started = Instant::now();
        assert!(matches!(
            attempt.finish_with_cancel(Duration::from_secs(60), || true),
            Err(Error::AuthorizationDenied)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            AuthorizationAttempt::begin(&config())
                .unwrap()
                .finish(Duration::ZERO),
            Err(Error::AuthorizationTimeout)
        ));
    }

    fn callback_streams() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        // Reproduce inherited nonblocking mode on every test platform.
        server.set_nonblocking(true).unwrap();
        (client, server)
    }

    #[test]
    fn callback_accepts_fragmented_headers_on_an_initially_nonblocking_stream() {
        use std::sync::mpsc::{self, RecvTimeoutError};
        let (mut client, mut server) = callback_streams();
        let (sent, received) = mpsc::channel();
        let worker = thread::spawn(move || {
            sent.send(read_callback(
                &mut server,
                "http://127.0.0.1:1234/callback",
                "expected",
            ))
            .unwrap();
        });
        // A connection may arrive before any request bytes. It is not invalid.
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        ));
        for chunk in [
            b"GET /callback?state=expec".as_slice(),
            b"ted&code=fragmented-code HTTP/1.1\r\nHost: localhost\r\n",
            b"\r",
        ] {
            client.write_all(chunk).unwrap();
            assert!(matches!(
                received.recv_timeout(Duration::from_millis(50)),
                Err(RecvTimeoutError::Timeout)
            ));
        }
        client.write_all(b"\n").unwrap();
        assert!(matches!(
            received.recv_timeout(Duration::from_secs(3)).unwrap(),
            Ok(Callback::Code(code)) if code == "fragmented-code"
        ));
        worker.join().unwrap();
    }

    #[test]
    fn callback_rejects_incomplete_and_oversized_headers() {
        let incomplete =
            b"GET /callback?state=expected&code=a HTTP/1.1\r\nHost: localhost\r\n".to_vec();
        let mut oversized =
            b"GET /callback?state=expected&code=a HTTP/1.1\r\nHost: localhost\r\nX-Padding: "
                .to_vec();
        oversized.extend(std::iter::repeat_n(b'x', 16 * 1024));
        oversized.extend_from_slice(b"\r\n\r\n");
        for request in [incomplete, oversized] {
            let (mut client, mut server) = callback_streams();
            client.write_all(&request).unwrap();
            client.shutdown(std::net::Shutdown::Write).unwrap();
            assert!(matches!(
                read_callback(&mut server, "http://127.0.0.1:1234/callback", "expected"),
                Err(Error::Protocol)
            ));
        }
    }

    #[test]
    fn callback_blocking_read_keeps_the_stalled_connection_timeout() {
        let (client, mut server) = callback_streams();
        let (sent, received) = std::sync::mpsc::channel();
        let started = Instant::now();
        let worker = thread::spawn(move || {
            sent.send(read_callback(
                &mut server,
                "http://127.0.0.1:1234/callback",
                "expected",
            ))
            .unwrap();
        });
        let result = received.recv_timeout(Duration::from_secs(6));
        let elapsed = started.elapsed();
        // Also release the peer before joining if the timeout ever regresses.
        drop(client);
        worker.join().unwrap();
        assert!(matches!(result.unwrap(), Err(Error::Protocol)));
        assert!(elapsed >= Duration::from_secs(1));
        assert!(elapsed < Duration::from_secs(6));
    }
}
