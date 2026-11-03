use std::collections::HashSet;

use reqwest::{
    StatusCode,
    blocking::{Client, RequestBuilder, Response},
};
use serde::{Deserialize, Serialize};

use crate::{Error, MAX_ENCRYPTED_BYTES, OAuthClientConfig, Result, Tokens, oauth};

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";
const MAX_LIST_PAGE: usize = 2 * 1024 * 1024;
const MAX_OBJECTS: usize = 100_000;
const MULTIPART_LIMIT: usize = 5_000_000;
const UPLOAD_CHUNK: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteObject {
    pub id: String,
    pub name: String,
    /// Drive returns the byte size as a decimal string; absent for non-file objects.
    pub size: Option<u64>,
    pub modified_time: Option<String>,
    pub md5_checksum: Option<String>,
}

pub trait ObjectStore {
    /// Complete enumeration. An incomplete response is an error, never an empty vault.
    fn list_objects(&mut self) -> Result<Vec<RemoteObject>>;
    fn read_object(&mut self, id: &str) -> Result<Vec<u8>>;
    /// Persist the returned identity before calling create_immutable.
    fn allocate_id(&mut self) -> Result<String>;
    /// A retry must supply the same id, name and bytes. Existing differing content
    /// is an integrity error; no operation overwrites an existing remote object.
    fn create_immutable(&mut self, id: &str, name: &str, bytes: &[u8]) -> Result<()>;
}

pub struct DriveClient {
    http: Client,
    config: OAuthClientConfig,
    tokens: Tokens,
    api: String,
    upload: String,
    token_url: String,
    cancelled: Option<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl DriveClient {
    pub fn new(config: OAuthClientConfig, tokens: Tokens) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            http: oauth::http_client()?,
            config,
            tokens,
            api: API.into(),
            upload: UPLOAD.into(),
            token_url: oauth::TOKEN_URL.into(),
            cancelled: None,
        })
    }

    /// Tokens can change on any operation, even one that subsequently fails.
    /// Persist this value in OS-protected storage after each operation.
    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    /// Install the connection generation fence. Cancellation is checked before
    /// each HTTP request, including resumable chunks, pages and read-back.
    pub fn set_cancellation(&mut self, cancelled: impl Fn() -> bool + Send + Sync + 'static) {
        self.cancelled = Some(Box::new(cancelled));
    }

    fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.as_ref().is_some_and(|cancelled| cancelled()) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn refresh_credentials(&mut self) -> Result<()> {
        self.check_cancelled()?;
        self.tokens = oauth::refresh(&self.http, &self.config, &self.tokens, &self.token_url)?;
        self.check_cancelled()?;
        Ok(())
    }

    /// Refresh only near expiry. Persist `tokens()` afterward, including when a
    /// subsequent Drive operation fails.
    pub fn ensure_credentials(&mut self) -> Result<()> {
        self.check_cancelled()?;
        if !self.tokens.valid() {
            self.refresh_credentials()?;
        }
        Ok(())
    }

    /// Stable account identity for local provider bindings. It is not placed in
    /// encrypted record payloads or filenames.
    pub fn account_id(&mut self) -> Result<String> {
        let url = format!("{}/about", self.api);
        let response =
            self.authorized(|http| http.get(&url).query(&[("fields", "user(permissionId)")]))?;
        let bytes = checked_response(response, 64 * 1024)?;
        #[derive(Deserialize)]
        struct About {
            user: User,
        }
        #[derive(Deserialize)]
        struct User {
            #[serde(rename = "permissionId")]
            permission_id: String,
        }
        let about: About = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        validate_id(&about.user.permission_id)?;
        Ok(about.user.permission_id)
    }

    fn authorized(&mut self, request: impl Fn(&Client) -> RequestBuilder) -> Result<Response> {
        self.check_cancelled()?;
        self.ensure_credentials()?;
        self.check_cancelled()?;
        let response = request(&self.http)
            .bearer_auth(&self.tokens.access_token)
            .send()
            .map_err(|_| Error::Retryable)?;
        self.check_cancelled()?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        drop(response);
        self.refresh_credentials()?;
        self.check_cancelled()?;
        let response = request(&self.http)
            .bearer_auth(&self.tokens.access_token)
            .send()
            .map_err(|_| Error::Retryable)?;
        self.check_cancelled()?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::ReconnectRequired);
        }
        Ok(response)
    }

    fn create_resumable(&mut self, id: &str, name: &str, bytes: &[u8]) -> Result<()> {
        let url = format!("{}/files", self.upload);
        let metadata = serde_json::json!({
            "id": id, "name": name, "parents": ["appDataFolder"], "mimeType": "application/octet-stream"
        });
        let response = self.authorized(|http| {
            http.post(&url)
                .query(&[("uploadType", "resumable"), ("fields", "id")])
                .header("X-Upload-Content-Type", "application/octet-stream")
                .header("X-Upload-Content-Length", bytes.len())
                .json(&metadata)
        })?;
        if response.status() == StatusCode::CONFLICT {
            return self.verify_uploaded(id, bytes);
        }
        if !response.status().is_success() {
            checked_response(response, 64 * 1024)?;
            return Err(Error::Protocol);
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(Error::Protocol)?
            .to_string();
        validate_upload_location(&self.upload, &location)?;
        drop(response);
        for start in (0..bytes.len()).step_by(UPLOAD_CHUNK) {
            let end = (start + UPLOAD_CHUNK).min(bytes.len());
            let response = self.authorized(|http| {
                http.put(&location)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .header(
                        reqwest::header::CONTENT_RANGE,
                        format!("bytes {start}-{}/{}", end - 1, bytes.len()),
                    )
                    .body(bytes[start..end].to_vec())
            })?;
            if matches!(response.status().as_u16(), 404 | 410) {
                // The resumable session expired, not an authored journal object.
                // Recreate the session next pass with the same durable file ID.
                return Err(Error::Retryable);
            }
            if response.status().as_u16() == 308 {
                // Drive acknowledges an exact contiguous prefix. A partial or
                // ambiguous response leaves the durable object queued; next run
                // starts a fresh session with the same immutable file identity.
                let range = response
                    .headers()
                    .get(reqwest::header::RANGE)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(Error::Retryable)?;
                if range != format!("bytes=0-{}", end - 1) || end == bytes.len() {
                    return Err(Error::Retryable);
                }
            } else if response.status() == StatusCode::CONFLICT {
                return self.verify_uploaded(id, bytes);
            } else {
                validate_created(response, id)?;
                if end != bytes.len() {
                    return Err(Error::Integrity);
                }
                return self.verify_uploaded(id, bytes);
            }
        }
        Err(Error::Retryable)
    }

    fn verify_uploaded(&mut self, id: &str, bytes: &[u8]) -> Result<()> {
        self.check_cancelled()?;
        if self.read_object(id)? != bytes {
            return Err(Error::Integrity);
        }
        Ok(())
    }
}

impl ObjectStore for DriveClient {
    fn list_objects(&mut self) -> Result<Vec<RemoteObject>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            files: Vec<File>,
            next_page_token: Option<String>,
            #[serde(default)]
            incomplete_search: bool,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct File {
            id: String,
            name: String,
            size: Option<String>,
            modified_time: Option<String>,
            md5_checksum: Option<String>,
        }
        let url = format!("{}/files", self.api);
        let mut result = Vec::new();
        let mut page_token = String::new();
        let mut seen_tokens = HashSet::new();
        let mut seen_ids = HashSet::new();
        loop {
            let mut query = vec![
                ("spaces", "appDataFolder"),
                ("q", "'appDataFolder' in parents and trashed = false"),
                ("pageSize", "1000"),
                (
                    "fields",
                    "nextPageToken,incompleteSearch,files(id,name,size,modifiedTime,md5Checksum)",
                ),
            ];
            if !page_token.is_empty() {
                query.push(("pageToken", &page_token));
            }
            let response = self.authorized(|http| http.get(&url).query(&query))?;
            let bytes = checked_response(response, MAX_LIST_PAGE)?;
            let page: Page = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
            if page.incomplete_search {
                return Err(Error::Retryable);
            }
            for file in page.files {
                validate_id(&file.id)?;
                if file.name.len() > 1024 || file.name.chars().any(char::is_control) {
                    return Err(Error::Protocol);
                }
                let size = file
                    .size
                    .map(|v| v.parse::<u64>().map_err(|_| Error::Protocol))
                    .transpose()?;
                // Duplicated file IDs across pages are harmless. Duplicated names
                // are retained because Drive does not enforce name uniqueness.
                if seen_ids.insert(file.id.clone()) {
                    result.push(RemoteObject {
                        id: file.id,
                        name: file.name,
                        size,
                        modified_time: file.modified_time,
                        md5_checksum: file.md5_checksum,
                    });
                }
                if result.len() > MAX_OBJECTS {
                    return Err(Error::ObjectTooLarge);
                }
            }
            match page.next_page_token {
                Some(token) if !token.is_empty() => {
                    if token.len() > 8192
                        || !seen_tokens.insert(token.clone())
                        || seen_tokens.len() > 1000
                    {
                        return Err(Error::Protocol);
                    }
                    page_token = token;
                }
                _ => return Ok(result),
            }
        }
    }

    fn read_object(&mut self, id: &str) -> Result<Vec<u8>> {
        validate_id(id)?;
        let url = format!("{}/files/{id}", self.api);
        let response = self.authorized(|http| {
            http.get(&url)
                .query(&[("alt", "media")])
                .timeout(std::time::Duration::from_secs(120))
        })?;
        checked_response(response, MAX_ENCRYPTED_BYTES)
    }

    fn allocate_id(&mut self) -> Result<String> {
        let url = format!("{}/files/generateIds", self.api);
        let response = self.authorized(|http| {
            http.get(&url).query(&[
                ("count", "1"),
                ("space", "appDataFolder"),
                ("type", "files"),
            ])
        })?;
        let bytes = checked_response(response, 64 * 1024)?;
        #[derive(Deserialize)]
        struct Ids {
            ids: Vec<String>,
        }
        let mut ids: Ids = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        if ids.ids.len() != 1 {
            return Err(Error::Protocol);
        }
        let id = ids.ids.remove(0);
        validate_id(&id)?;
        Ok(id)
    }

    fn create_immutable(&mut self, id: &str, name: &str, bytes: &[u8]) -> Result<()> {
        validate_id(id)?;
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(Error::Protocol);
        }
        if bytes.len() > MAX_ENCRYPTED_BYTES {
            return Err(Error::ObjectTooLarge);
        }
        if bytes.len() > MULTIPART_LIMIT {
            return self.create_resumable(id, name, bytes);
        }
        let boundary = format!("rhythm_{}", uuid::Uuid::new_v4().simple());
        let metadata = serde_json::to_vec(&serde_json::json!({
            "id": id, "name": name, "parents": ["appDataFolder"], "mimeType": "application/octet-stream"
        })).map_err(|_| Error::Protocol)?;
        let mut body = Vec::with_capacity(bytes.len() + metadata.len() + 512);
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&metadata);
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = format!("{}/files", self.upload);
        let response = self.authorized(|http| {
            http.post(&url)
                .query(&[("uploadType", "multipart"), ("fields", "id")])
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/related; boundary={boundary}"),
                )
                .body(body.clone())
        })?;
        let conflict = response.status() == StatusCode::CONFLICT;
        if !conflict {
            validate_created(response, id)?;
        }
        // An upload's ambiguous timeout is retried with this same ID. A 409 alone
        // never proves delivery: validate the bytes already stored under it.
        self.verify_uploaded(id, bytes)
    }
}

fn validate_created(response: Response, id: &str) -> Result<()> {
    let response_bytes = checked_response(response, 64 * 1024)?;
    #[derive(Deserialize)]
    struct Created {
        id: String,
    }
    let created: Created = serde_json::from_slice(&response_bytes).map_err(|_| Error::Protocol)?;
    if created.id != id {
        return Err(Error::Integrity);
    }
    Ok(())
}

fn validate_upload_location(upload: &str, location: &str) -> Result<()> {
    let expected = url::Url::parse(upload).map_err(|_| Error::Protocol)?;
    let actual = url::Url::parse(location).map_err(|_| Error::Protocol)?;
    // Never forward the bearer token to another host, a changed scheme/port,
    // or a URL containing credentials supplied in a response header.
    if actual.origin() != expected.origin()
        || !actual.username().is_empty()
        || actual.password().is_some()
        || actual.fragment().is_some()
        || location.len() > 8192
    {
        return Err(Error::Protocol);
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 512
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::Protocol);
    }
    Ok(())
}

fn checked_response(response: Response, limit: usize) -> Result<Vec<u8>> {
    let status = response.status();
    if status.is_success() {
        return oauth::read_bounded(response, limit);
    }
    let bytes = oauth::read_bounded(response, 64 * 1024)?;
    if status == StatusCode::UNAUTHORIZED {
        return Err(Error::ReconnectRequired);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(Error::MissingObject);
    }
    if status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
    {
        return Err(Error::Retryable);
    }
    if status == StatusCode::FORBIDDEN {
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        let reasons = value["error"]["errors"].as_array();
        if reasons
            .is_some_and(|values| values.iter().any(|v| v["reason"] == "storageQuotaExceeded"))
        {
            return Err(Error::Quota);
        }
        if reasons.is_some_and(|values| {
            values.iter().any(|v| {
                v["reason"] == "rateLimitExceeded" || v["reason"] == "userRateLimitExceeded"
            })
        }) {
            return Err(Error::Retryable);
        }
    }
    Err(Error::Remote(status.as_u16()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    fn mock<B: AsRef<str> + Send + 'static>(
        responses: Vec<(u16, B)>,
    ) -> (DriveClient, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        mock_with_cancellation(responses, None)
    }

    fn mock_with_cancellation<B: AsRef<str> + Send + 'static>(
        responses: Vec<(u16, B)>,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> (DriveClient, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let saved = Arc::clone(&requests);
        let server_base = base.clone();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let body = body.as_ref();
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(index) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..index]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= index + 4 + length {
                            break;
                        }
                    }
                }
                saved
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes).into_owned());
                if status == 998 {
                    // The fake store committed the captured body, but the
                    // response was lost after the client's request deadline.
                    thread::sleep(std::time::Duration::from_millis(250));
                    continue;
                }
                if status == 997 {
                    write!(stream, "HTTP/1.1 200 OK\r\nLocation: {server_base}/upload-session\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    continue;
                }
                if status == 308 || status == 996 {
                    if status == 996 {
                        cancelled.as_ref().unwrap().store(true, Ordering::Release);
                    }
                    write!(stream, "HTTP/1.1 308 Resume Incomplete\r\nRange: {body}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    continue;
                }
                write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let mut client = DriveClient::new(
            OAuthClientConfig {
                client_id: "123456789-example.apps.googleusercontent.com".into(),
                client_secret: None,
            },
            Tokens {
                access_token: "test-token".into(),
                refresh_token: "test-refresh".into(),
                expires_at_unix_s: oauth::now() + 3600,
            },
        )
        .unwrap();
        client.api = base.clone();
        client.upload = base.clone();
        client.token_url = format!("{base}/token");
        (client, requests, handle)
    }

    #[test]
    fn list_reads_every_page_and_retains_duplicate_names() {
        let (mut client, requests, handle) = mock(vec![
            (
                200,
                r#"{"files":[{"id":"a","name":"same","size":"12","md5Checksum":"a1"}],"nextPageToken":"next"}"#,
            ),
            (200, r#"{"files":[{"id":"b","name":"same","size":"13"}]}"#),
        ]);
        let objects = client.list_objects().unwrap();
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].size, Some(12));
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("spaces=appDataFolder"));
        assert!(requests[1].contains("pageToken=next"));
    }

    #[test]
    fn ambiguous_create_conflict_requires_matching_readback() {
        let (mut client, requests, handle) = mock(vec![(409, "{}"), (200, "payload")]);
        client
            .create_immutable("fixed-id", "object", b"payload")
            .unwrap();
        handle.join().unwrap();
        assert!(requests.lock().unwrap()[0].contains("\"id\":\"fixed-id\""));
        let (mut client, _, handle) = mock(vec![(409, "{}"), (200, "different")]);
        assert!(matches!(
            client.create_immutable("fixed-id", "object", b"payload"),
            Err(Error::Integrity)
        ));
        handle.join().unwrap();
    }

    #[test]
    fn committed_upload_timeout_retries_same_identity_and_frozen_bytes() {
        let (mut client, requests, handle) =
            mock(vec![(998, ""), (409, "{}"), (200, "frozen-ciphertext")]);
        client.http = Client::builder()
            .timeout(std::time::Duration::from_millis(100))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert!(matches!(
            client.create_immutable("durable-upload-id", "object", b"frozen-ciphertext"),
            Err(Error::Retryable)
        ));
        thread::sleep(std::time::Duration::from_millis(200));
        client
            .create_immutable("durable-upload-id", "object", b"frozen-ciphertext")
            .unwrap();
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in &requests[..2] {
            assert!(request.contains("\"id\":\"durable-upload-id\""));
            assert!(request.contains("\r\n\r\nfrozen-ciphertext\r\n"));
        }
    }

    #[test]
    fn resumable_location_cannot_send_tokens_to_another_origin() {
        assert!(
            validate_upload_location(
                UPLOAD,
                "https://www.googleapis.com/upload/drive/v3/files?upload_id=opaque"
            )
            .is_ok()
        );
        for location in [
            "https://attacker.test/upload",
            "http://www.googleapis.com/upload",
            "https://www.googleapis.com:8443/upload",
            "https://user@www.googleapis.com/upload",
            "https://www.googleapis.com/upload#fragment",
        ] {
            assert!(validate_upload_location(UPLOAD, location).is_err());
        }
    }

    #[test]
    fn large_upload_conflict_verifies_same_file_instead_of_replacing_it() {
        let (mut client, requests, handle) = mock(vec![(409, "{}"), (200, "incorrect-content")]);
        assert!(matches!(
            client.create_immutable("large-id", "object", &vec![7; MULTIPART_LIMIT + 1]),
            Err(Error::Integrity)
        ));
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("uploadType=resumable"));
        assert!(requests[0].contains("\"id\":\"large-id\""));
        assert!(!requests[0].contains("multipart/related"));
    }

    #[test]
    fn large_upload_uses_contiguous_bounded_chunks_then_readback() {
        let payload = "x".repeat(MULTIPART_LIMIT + 1);
        let mut replies = vec![(997, String::new())];
        for start in (0..payload.len()).step_by(UPLOAD_CHUNK) {
            let end = (start + UPLOAD_CHUNK).min(payload.len());
            if end == payload.len() {
                replies.push((200, r#"{"id":"large-id"}"#.to_string()));
            } else {
                replies.push((308, format!("bytes=0-{}", end - 1)));
            }
        }
        replies.push((200, payload.clone()));
        let (mut client, requests, handle) = mock(replies);
        client
            .create_immutable("large-id", "object", payload.as_bytes())
            .unwrap();
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        let mut rebuilt = String::new();
        for (index, request) in requests[1..requests.len() - 1].iter().enumerate() {
            let (headers, body) = request.split_once("\r\n\r\n").unwrap();
            assert!(headers.starts_with("PUT /upload-session HTTP/1.1"));
            assert!(headers.to_ascii_lowercase().contains(&format!(
                "content-range: bytes {}-{}/{}",
                index * UPLOAD_CHUNK,
                (index * UPLOAD_CHUNK + body.len()) - 1,
                payload.len()
            )));
            assert!(body.len() <= UPLOAD_CHUNK);
            rebuilt.push_str(body);
        }
        assert_eq!(rebuilt, payload);
    }

    #[test]
    fn expired_upload_session_and_http_timeout_are_retryable() {
        let (mut client, _, handle) = mock(vec![(997, ""), (404, "{}")]);
        assert!(matches!(
            client.create_immutable("large-id", "object", &vec![7; MULTIPART_LIMIT + 1]),
            Err(Error::Retryable)
        ));
        handle.join().unwrap();
        let (mut client, _, handle) = mock(vec![(408, "{}")]);
        assert!(matches!(client.list_objects(), Err(Error::Retryable)));
        handle.join().unwrap();
    }

    #[test]
    fn cancellation_after_first_chunk_stops_large_upload_before_next_request() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (mut client, requests, handle) = mock_with_cancellation(
            vec![(997, ""), (996, "bytes=0-1048575")],
            Some(Arc::clone(&cancelled)),
        );
        client.set_cancellation(move || cancelled.load(Ordering::Acquire));
        assert!(matches!(
            client.create_immutable("large-id", "object", &vec![7; MULTIPART_LIMIT + 1]),
            Err(Error::Cancelled)
        ));
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("uploadType=resumable"));
        assert!(requests[1].starts_with("PUT /upload-session"));
    }

    #[test]
    fn cancelled_client_does_not_refresh_credentials_or_touch_provider() {
        let (mut client, requests, handle) = mock(Vec::<(u16, &str)>::new());
        client.tokens.expires_at_unix_s = 0;
        client.set_cancellation(|| true);
        assert!(matches!(client.list_objects(), Err(Error::Cancelled)));
        assert!(matches!(
            client.refresh_credentials(),
            Err(Error::Cancelled)
        ));
        handle.join().unwrap();
        assert!(requests.lock().unwrap().is_empty());
    }

    #[test]
    fn account_identity_and_reserved_upload_ids_use_appdata_scope() {
        let (mut client, requests, handle) = mock(vec![
            (200, r#"{"user":{"permissionId":"user-123"}}"#),
            (200, r#"{"ids":["reserved-id"]}"#),
        ]);
        assert_eq!(client.account_id().unwrap(), "user-123");
        assert_eq!(client.allocate_id().unwrap(), "reserved-id");
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("fields=user%28permissionId%29"));
        assert!(requests[1].contains("space=appDataFolder"));
    }

    #[test]
    fn unauthorized_refreshes_once_and_preserves_refresh_token() {
        let (mut client, requests, handle) = mock(vec![
            (401, "{}"),
            (
                200,
                r#"{"access_token":"new-token","token_type":"Bearer","expires_in":3600}"#,
            ),
            (200, "payload"),
        ]);
        assert_eq!(client.read_object("one").unwrap(), b"payload");
        assert_eq!(client.tokens().refresh_token, "test-refresh");
        handle.join().unwrap();
        assert!(requests.lock().unwrap()[2].contains("Bearer new-token"));
    }

    #[test]
    fn revoked_access_pauses_and_missing_data_is_not_empty() {
        let (mut client, _, handle) =
            mock(vec![(401, "{}"), (400, r#"{"error":"invalid_grant"}"#)]);
        assert!(matches!(
            client.read_object("one"),
            Err(Error::ReconnectRequired)
        ));
        handle.join().unwrap();
        let (mut client, _, handle) = mock(vec![(404, "{}")]);
        assert!(matches!(
            client.read_object("one"),
            Err(Error::MissingObject)
        ));
        handle.join().unwrap();
    }

    #[test]
    fn incomplete_or_repeated_pagination_is_not_reported_as_complete() {
        let (mut client, _, handle) = mock(vec![(200, r#"{"files":[],"incompleteSearch":true}"#)]);
        assert!(matches!(client.list_objects(), Err(Error::Retryable)));
        handle.join().unwrap();
        let (mut client, _, handle) = mock(vec![
            (200, r#"{"files":[],"nextPageToken":"again"}"#),
            (200, r#"{"files":[],"nextPageToken":"again"}"#),
        ]);
        assert!(matches!(client.list_objects(), Err(Error::Protocol)));
        handle.join().unwrap();
    }
}
