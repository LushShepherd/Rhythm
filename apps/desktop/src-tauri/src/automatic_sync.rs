//! Native background orchestration, separate from both the WebView and SQLite worker.
use crate::{
    protected_sync,
    sync_engine::{self, Failure, Host, LocalSnapshot, PendingUpload, SyncState},
};
use rhythm_desktop_service::{SyncArchive, SyncImportReport};
use rhythm_sync::{
    AuthorizationAttempt, DriveClient, OAuthClientConfig, ObjectStore, Tokens, VaultDescriptor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const CONNECTION_FILE: &str = "google-sync.protected";
const BUNDLED_OAUTH: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/google-oauth-client.json"));

#[derive(Clone, Serialize)]
pub struct Status {
    pub state: String,
    pub connected: bool,
    pub reconnect_required: bool,
    pub last_success_at_utc_ms: Option<i64>,
    pub error: Option<String>,
}

impl Status {
    fn new(state: &str, connected: bool) -> Self {
        Self {
            state: state.into(),
            connected,
            reconnect_required: false,
            last_success_at_utc_ms: None,
            error: None,
        }
    }

    fn failed(
        failure: &Failure,
        connected: bool,
        last_success_at_utc_ms: Option<i64>,
        automatic_retry: bool,
    ) -> Self {
        let mut message = failure.message.clone();
        if failure.retryable {
            if !connected {
                message.push_str(
                    " Check your network connection, then choose Connect Google Drive again.",
                );
            } else if automatic_retry {
                message.push_str(" Rhythm will retry while it is open.");
            }
        }
        Self {
            state: "error".into(),
            connected,
            reconnect_required: failure.reconnect_required,
            last_success_at_utc_ms,
            error: Some(message),
        }
    }
}

#[derive(Clone)]
pub struct Cancellation {
    generation: Arc<AtomicU64>,
    epoch: u64,
}
impl Cancellation {
    pub fn cancelled(&self) -> bool {
        self.generation.load(Ordering::Acquire) != self.epoch
    }
}

pub trait Journal: Send + 'static {
    fn snapshot(&mut self) -> Result<LocalSnapshot, String>;
    fn import(
        &mut self,
        device_id: &str,
        archive: &SyncArchive,
        cancellation: &Cancellation,
    ) -> Result<SyncImportReport, String>;
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    version: u32,
    client_id: String,
    account_id: String,
    tokens: Tokens,
    sync: SyncState,
}

enum Action {
    Connect(Zeroizing<String>, u64, mpsc::Sender<Result<Status, String>>),
    Retry(mpsc::Sender<Result<Status, String>>),
    Disconnect(mpsc::Sender<Result<Status, String>>),
}

#[derive(Clone)]
pub struct Manager {
    sender: mpsc::Sender<Action>,
    status: Arc<Mutex<Status>>,
    generation: Arc<AtomicU64>,
}

impl Manager {
    pub fn start(
        directory: PathBuf,
        journal: impl Journal,
        status_event: impl Fn(Status) + Send + Sync + 'static,
        history_event: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let configuration = load_configuration();
        let initial = Status::new(
            if matches!(configuration, Ok(Some(_))) {
                "disconnected"
            } else {
                "not_configured"
            },
            false,
        );
        let status = Arc::new(Mutex::new(initial));
        let generation = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::channel();
        let shared = status.clone();
        let worker_generation = generation.clone();
        thread::Builder::new()
            .name("rhythm-google-sync".into())
            .spawn(move || {
                let mut worker = Worker {
                    directory,
                    journal: Box::new(journal),
                    status: shared,
                    generation: worker_generation,
                    status_event: Box::new(status_event),
                    history_event: Box::new(history_event),
                    configuration,
                };
                worker.run(receiver);
            })
            .map_err(|_| "The background sync worker could not start.")?;
        Ok(Self {
            sender,
            status,
            generation,
        })
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn connect(&self, passphrase: String) -> Result<Status, String> {
        let passphrase = Zeroizing::new(passphrase);
        if passphrase.chars().count() < 12 || passphrase.len() > 1024 {
            return Err(
                "Use a recovery passphrase of at least 12 characters, up to 1,024 UTF-8 bytes."
                    .into(),
            );
        }
        let epoch = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.call(|reply| Action::Connect(passphrase, epoch, reply))
    }

    pub fn retry(&self) -> Result<Status, String> {
        self.call(Action::Retry)
    }

    pub fn disconnect(&self) -> Result<Status, String> {
        // Also fences queued imports on the storage worker, and cancels OAuth waiting.
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.call(Action::Disconnect)
    }

    fn call(
        &self,
        action: impl FnOnce(mpsc::Sender<Result<Status, String>>) -> Action,
    ) -> Result<Status, String> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(action(reply))
            .map_err(|_| "The background sync worker stopped. Reopen Rhythm.")?;
        response
            .recv()
            .map_err(|_| "The sync operation did not finish. Local writing is safe.")?
    }
}

fn load_configuration() -> Result<Option<OAuthClientConfig>, String> {
    let bytes = if let Some(path) = std::env::var_os("RHYTHM_GOOGLE_OAUTH_CLIENT_FILE") {
        let path = PathBuf::from(path);
        if fs::metadata(&path)
            .map_err(|_| "Rhythm's Google registration file could not be read.")?
            .len()
            > 64 * 1024
        {
            return Err("Rhythm's Google registration file is invalid.".into());
        }
        fs::read(path).map_err(|_| "Rhythm's Google registration file could not be read.")?
    } else {
        BUNDLED_OAUTH.to_vec()
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    OAuthClientConfig::from_google_json(&bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn save_connection(directory: &Path, connection: &Connection) -> Result<(), String> {
    let plain = Zeroizing::new(
        serde_json::to_vec(connection).map_err(|_| "The sync connection could not be encoded.")?,
    );
    protected_sync::write(&directory.join(CONNECTION_FILE), &plain)
}

fn pending_path(directory: &Path, remote_id: &str) -> Result<PathBuf, String> {
    if remote_id.is_empty()
        || remote_id.len() > 256
        || !remote_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("A pending upload identity is invalid.".into());
    }
    Ok(directory
        .join("google-sync-outbox")
        .join(format!("{remote_id}.pending")))
}

fn save_pending(directory: &Path, remote_id: &str, bytes: &[u8]) -> Result<(), String> {
    let path = pending_path(directory, remote_id)?;
    let parent = path.parent().ok_or("Missing sync outbox directory.")?;
    fs::create_dir_all(parent).map_err(|_| "The encrypted sync outbox could not be opened.")?;
    if path.exists() {
        let existing = read_pending(directory, remote_id)?;
        return if existing == bytes {
            Ok(())
        } else {
            Err("A pending encrypted upload has changed. Sync is paused.".into())
        };
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "The encrypted upload could not be saved.")?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| "The encrypted upload could not be saved.")?;
    file.persist_noclobber(path)
        .map_err(|_| "The encrypted upload could not be saved safely.")?;
    Ok(())
}

fn read_pending(directory: &Path, remote_id: &str) -> Result<Vec<u8>, String> {
    let path = pending_path(directory, remote_id)?;
    let metadata = fs::metadata(&path)
        .map_err(|_| "A pending encrypted upload is missing. Sync is paused for recovery.")?;
    if !metadata.is_file() || metadata.len() > 96 * 1024 * 1024 {
        return Err("A pending encrypted upload is invalid.".into());
    }
    fs::read(path).map_err(|_| "A pending encrypted upload could not be read.".into())
}

struct Worker {
    directory: PathBuf,
    journal: Box<dyn Journal>,
    status: Arc<Mutex<Status>>,
    generation: Arc<AtomicU64>,
    status_event: Box<dyn Fn(Status) + Send + Sync>,
    history_event: Box<dyn Fn() + Send + Sync>,
    configuration: Result<Option<OAuthClientConfig>, String>,
}

impl Worker {
    fn publish(&self, status: Status) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        (self.status_event)(status);
    }

    fn current(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn failure(
        &self,
        failure: &Failure,
        connection: Option<&Connection>,
        automatic_retry: bool,
    ) -> String {
        let status = Status::failed(
            failure,
            connection.is_some(),
            connection.and_then(|c| c.sync.last_success_at_utc_ms),
            automatic_retry,
        );
        let message = status.error.clone().unwrap_or_default();
        self.publish(status);
        message
    }

    fn read_connection(&self) -> Result<Option<Connection>, String> {
        let Some(plain) = protected_sync::read(&self.directory.join(CONNECTION_FILE))? else {
            return Ok(None);
        };
        let value: Connection = serde_json::from_slice(&plain).map_err(
            |_| "The protected sync connection is invalid. Disconnect and reconnect to recover.",
        )?;
        if value.version != 1 || value.sync.descriptor.vault_id != value.sync.key.vault_id() {
            return Err("This saved sync connection requires a different Rhythm version.".into());
        }
        Ok(Some(value))
    }

    fn run(&mut self, receiver: mpsc::Receiver<Action>) {
        let mut connection = match self.read_connection() {
            Ok(value) => value,
            Err(message) => {
                let mut status = Status::new("error", true);
                status.error = Some(message);
                self.publish(status);
                None
            }
        };
        if let Some(current) = &connection {
            let mut status = Status::new("syncing", true);
            status.last_success_at_utc_ms = current.sync.last_success_at_utc_ms;
            self.publish(status);
        }
        if let Err(message) = &self.configuration {
            let mut status = Status::new("not_configured", connection.is_some());
            status.error = Some(message.clone());
            self.publish(status);
        } else if matches!(self.configuration, Ok(None)) {
            self.publish(Status::new("not_configured", connection.is_some()));
        }
        let mut next = Instant::now();
        let mut failures = 0_u32;
        let mut paused = !matches!(self.configuration, Ok(Some(_)));
        loop {
            let delay = if connection.is_none() || paused {
                Duration::from_secs(60)
            } else {
                next.saturating_duration_since(Instant::now())
            };
            match receiver.recv_timeout(delay) {
                Ok(Action::Connect(passphrase, epoch, reply)) => {
                    let result = self.connect(&passphrase, epoch, connection.as_ref());
                    drop(passphrase);
                    match result {
                        Ok(value) => {
                            connection = Some(value);
                            paused = false;
                            failures = 0;
                            next = Instant::now();
                            self.publish(Status::new("syncing", true));
                            let _ = reply.send(Ok(self.current()));
                        }
                        Err(error) => {
                            // First enrollment has no saved connection to retry.
                            // A failed reconnect retains the previous scheduling state.
                            let message = self.failure(&error, connection.as_ref(), !paused);
                            let _ = reply.send(Err(message));
                        }
                    }
                }
                Ok(Action::Retry(reply)) => {
                    if connection.is_some() && matches!(self.configuration, Ok(Some(_))) {
                        paused = false;
                        next = Instant::now();
                        let mut status = self.current();
                        status.state = "syncing".into();
                        status.error = None;
                        self.publish(status);
                        let _ = reply.send(Ok(self.current()));
                    } else {
                        let _ =
                            reply.send(Err("Connect Google Drive before retrying sync.".into()));
                    }
                }
                Ok(Action::Disconnect(reply)) => {
                    match protected_sync::remove(&self.directory.join(CONNECTION_FILE)) {
                        Ok(()) => {
                            if let Some(pending) =
                                connection.take().and_then(|current| current.sync.pending)
                                && let Ok(path) = pending_path(&self.directory, &pending.remote_id)
                            {
                                let _ = fs::remove_file(path);
                            }
                            paused = false;
                            self.publish(Status::new(
                                if matches!(self.configuration, Ok(Some(_))) {
                                    "disconnected"
                                } else {
                                    "not_configured"
                                },
                                false,
                            ));
                            let _ = reply.send(Ok(self.current()));
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if paused {
                        continue;
                    }
                    let Some(current) = connection.as_mut() else {
                        continue;
                    };
                    let epoch = self.generation.load(Ordering::Acquire);
                    let mut status = self.current();
                    status.state = "syncing".into();
                    status.error = None;
                    self.publish(status);
                    match self.synchronize(current, epoch) {
                        Ok(complete) => {
                            failures = 0;
                            let mut status =
                                Status::new(if complete { "ready" } else { "syncing" }, true);
                            status.last_success_at_utc_ms = current.sync.last_success_at_utc_ms;
                            self.publish(status);
                            next =
                                Instant::now() + Duration::from_secs(if complete { 60 } else { 1 });
                        }
                        Err(error) => {
                            if self.generation.load(Ordering::Acquire) != epoch {
                                continue;
                            }
                            self.failure(&error, Some(current), error.retryable);
                            failures = failures.saturating_add(1);
                            paused = !error.retryable;
                            // Bounded exponential backoff with independent device jitter.
                            let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0] % 16);
                            next = Instant::now()
                                + Duration::from_secs(
                                    (15_u64.saturating_mul(1 << failures.min(5))).min(300) + jitter,
                                );
                        }
                    }
                }
            }
        }
    }

    fn connect(
        &mut self,
        passphrase: &str,
        epoch: u64,
        previous: Option<&Connection>,
    ) -> Result<Connection, Failure> {
        let config = self.configuration.clone()?.ok_or_else(|| {
            Failure::from("Google Drive connection is not configured in this build.".to_string())
        })?;
        if previous.is_some_and(|c| c.client_id != config.client_id) {
            return Err(
                "Rhythm's Google registration changed. Disconnect the previous connection first."
                    .to_string()
                    .into(),
            );
        }
        self.publish(Status::new("connecting", previous.is_some()));
        let cancellation = Cancellation {
            generation: self.generation.clone(),
            epoch,
        };
        let authorization = AuthorizationAttempt::begin(&config)?;
        protected_sync::open_google_authorization(authorization.authorization_url())?;
        let tokens = authorization
            .finish_with_cancel(Duration::from_secs(180), || cancellation.cancelled())?;
        let mut provider = DriveClient::new(config.clone(), tokens)?;
        let network_cancellation = cancellation.clone();
        provider.set_cancellation(move || network_cancellation.cancelled());
        let account_id = provider.account_id()?;
        if previous.is_some_and(|c| c.account_id != account_id) {
            return Err("This is a different Google account. Disconnect first if you intend to switch accounts.".to_string().into());
        }
        let remote = provider.list_objects()?;
        let descriptor = sync_engine::descriptors(&remote)?;
        let local = self.journal.snapshot()?;
        let sync = if let Some(remote_descriptor) = descriptor {
            let bytes = provider.read_object(&remote_descriptor.id)?;
            let descriptor: VaultDescriptor = serde_json::from_slice(&bytes)
                .map_err(|_| Failure::from(rhythm_sync::Error::Protocol))?;
            if previous.is_some_and(|c| {
                c.sync.descriptor != descriptor || c.sync.descriptor_id != remote_descriptor.id
            }) {
                return Err("The previous Drive collection changed. Local writing is safe; reconnect only after checking your recovery copy.".to_string().into());
            }
            let key = descriptor.unlock(passphrase)?;
            if let Some(previous) = previous {
                let mut sync = previous.sync.clone();
                sync.key = key;
                sync
            } else {
                SyncState {
                    descriptor,
                    descriptor_id: remote_descriptor.id.clone(),
                    key,
                    device_id: local.device_id,
                    seen: BTreeMap::new(),
                    pending: None,
                    last_success_at_utc_ms: None,
                }
            }
        } else if let Some(previous) = previous {
            // A first-ever descriptor upload may have been staged before a crash.
            if previous
                .sync
                .pending
                .as_ref()
                .is_some_and(|p| p.name == sync_engine::DESCRIPTOR_NAME)
            {
                let _ = previous.sync.descriptor.unlock(passphrase)?;
                previous.sync.clone()
            } else {
                return Err("The previous sync collection is missing from Drive. Local writing is safe; sync is paused for recovery.".to_string().into());
            }
        } else {
            if remote
                .iter()
                .any(|object| object.name.starts_with("rhythm-"))
            {
                return Err("Drive contains Rhythm data without its recovery information. Sync is paused to preserve it.".to_string().into());
            }
            let (descriptor, key) = VaultDescriptor::create(passphrase)?;
            let remote_id = provider.allocate_id()?;
            let bytes = serde_json::to_vec(&descriptor)
                .map_err(|_| Failure::from(rhythm_sync::Error::Protocol))?;
            save_pending(&self.directory, &remote_id, &bytes)?;
            SyncState {
                descriptor,
                descriptor_id: remote_id.clone(),
                key,
                device_id: local.device_id,
                seen: BTreeMap::new(),
                last_success_at_utc_ms: None,
                pending: Some(PendingUpload {
                    remote_id,
                    name: sync_engine::DESCRIPTOR_NAME.into(),
                    ciphertext_hash: sync_engine::digest(&bytes),
                }),
            }
        };
        if cancellation.cancelled() {
            return Err("Google Drive connection was cancelled.".to_string().into());
        }
        let connection = Connection {
            version: 1,
            client_id: config.client_id.clone(),
            account_id,
            tokens: provider.tokens().clone(),
            sync,
        };
        save_connection(&self.directory, &connection)?;
        Ok(connection)
    }

    fn synchronize(&mut self, connection: &mut Connection, epoch: u64) -> Result<bool, Failure> {
        let config = self.configuration.clone()?.ok_or_else(|| {
            Failure::from("Google Drive connection is not configured in this build.".to_string())
        })?;
        if connection.client_id != config.client_id {
            return Err("The Google application registration changed. Disconnect and reconnect before syncing.".to_string().into());
        }
        let mut provider = DriveClient::new(config, connection.tokens.clone())?;
        let cancellation = Cancellation {
            generation: self.generation.clone(),
            epoch,
        };
        let network_cancellation = cancellation.clone();
        provider.set_cancellation(move || network_cancellation.cancelled());
        let result = (|| {
            provider.ensure_credentials()?;
            connection.tokens = provider.tokens().clone();
            if provider.account_id()? != connection.account_id {
                return Err(Failure::from(
                    "The Google account changed. Sync is paused.".to_string(),
                ));
            }
            let mut sync = connection.sync.clone();
            let mut host = WorkerHost {
                directory: &self.directory,
                journal: &mut *self.journal,
                connection,
                cancellation: cancellation.clone(),
                history_event: &*self.history_event,
            };
            let result = sync_engine::synchronize(&mut provider, &mut sync, &mut host);
            connection.sync = sync;
            result
        })();
        connection.tokens = provider.tokens().clone();
        if !cancellation.cancelled() {
            if matches!(result, Ok(true)) {
                connection.sync.last_success_at_utc_ms = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .min(i64::MAX as u128) as i64,
                );
            }
            save_connection(&self.directory, connection)?;
        }
        result
    }
}

struct WorkerHost<'a> {
    directory: &'a Path,
    journal: &'a mut dyn Journal,
    connection: &'a mut Connection,
    cancellation: Cancellation,
    history_event: &'a (dyn Fn() + Send + Sync),
}

impl Host for WorkerHost<'_> {
    fn snapshot(&mut self) -> Result<LocalSnapshot, String> {
        self.journal.snapshot()
    }
    fn import(
        &mut self,
        device_id: &str,
        archive: &SyncArchive,
    ) -> Result<SyncImportReport, String> {
        self.journal.import(device_id, archive, &self.cancellation)
    }
    fn checkpoint(&mut self, state: &SyncState) -> Result<(), String> {
        if self.cancelled() {
            return Err("Sync was stopped.".into());
        }
        self.connection.sync = state.clone();
        save_connection(self.directory, self.connection)
    }
    fn save_pending(&mut self, remote_id: &str, bytes: &[u8]) -> Result<(), String> {
        save_pending(self.directory, remote_id, bytes)
    }
    fn read_pending(&mut self, remote_id: &str) -> Result<Vec<u8>, String> {
        read_pending(self.directory, remote_id)
    }
    fn remove_pending(&mut self, remote_id: &str) {
        if let Ok(path) = pending_path(self.directory, remote_id) {
            let _ = fs::remove_file(path);
        }
    }
    fn cancelled(&self) -> bool {
        self.cancellation.cancelled()
    }
    fn history_changed(&self) {
        (self.history_event)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_first_enrollment_requires_an_explicit_connect_instead_of_promising_retry() {
        let failure = Failure::from(rhythm_sync::Error::Retryable);
        // Even if the scheduler was otherwise active, no saved connection exists.
        let status = Status::failed(&failure, false, None, true);
        assert_eq!(status.state, "error");
        assert!(!status.connected);
        assert!(!status.reconnect_required);
        assert_eq!(status.last_success_at_utc_ms, None);
        let message = status.error.unwrap();
        assert!(message.contains("choose Connect Google Drive again"));
        assert!(!message.contains("will retry"));
    }

    #[test]
    fn saved_connection_only_promises_retry_when_the_worker_will_retry() {
        let failure = Failure::from(rhythm_sync::Error::Retryable);
        let retrying = Status::failed(&failure, true, Some(123), true);
        assert!(retrying.connected);
        assert!(!retrying.reconnect_required);
        assert_eq!(retrying.last_success_at_utc_ms, Some(123));
        assert!(
            retrying
                .error
                .unwrap()
                .contains("will retry while it is open")
        );
        let paused = Status::failed(&failure, true, Some(123), false);
        assert!(paused.connected);
        assert_eq!(paused.error.as_deref(), Some(failure.message.as_str()));
    }

    #[test]
    fn revoked_access_and_cancellation_keep_their_original_recovery_behavior() {
        for error in [
            rhythm_sync::Error::ReconnectRequired,
            rhythm_sync::Error::Cancelled,
            rhythm_sync::Error::AuthorizationDenied,
        ] {
            let failure = Failure::from(error);
            for connected in [false, true] {
                let status = Status::failed(&failure, connected, None, true);
                assert_eq!(status.connected, connected);
                assert_eq!(status.reconnect_required, failure.reconnect_required);
                assert_eq!(status.error.as_deref(), Some(failure.message.as_str()));
                assert!(!status.error.unwrap().contains("will retry"));
            }
        }
    }
}
