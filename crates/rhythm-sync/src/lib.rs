//! Direct Google Drive object transport and desktop OAuth.
//!
//! This crate never opens the user's database or persists credentials. Callers must
//! durably retain upload identities before sending and protect tokens/recovery keys
//! using the operating system's credential protection. Network I/O belongs on a
//! separate worker, never the timer/storage worker.

mod crypto;
mod drive;
mod oauth;

pub use crypto::{VaultDescriptor, VaultKey, decrypt_object, encrypt_object};
pub use drive::{DriveClient, ObjectStore, RemoteObject};
pub use oauth::{AuthorizationAttempt, OAuthClientConfig, Tokens};

pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENCRYPTED_BYTES: usize = 96 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Sync was stopped. Local writing is safe.")]
    Cancelled,
    #[error("Rhythm's Google desktop application registration is unavailable or invalid.")]
    Configuration,
    #[error("Google sign-in was cancelled.")]
    AuthorizationDenied,
    #[error("Google sign-in timed out. Try connecting again.")]
    AuthorizationTimeout,
    #[error("Reconnect Google Drive to continue syncing. Your local writing is safe.")]
    ReconnectRequired,
    #[error("Google Drive has no space available. Your local writing is safe.")]
    Quota,
    #[error("Rhythm could not reach Google Drive or complete the request right now.")]
    Retryable,
    #[error("Google Drive could not complete this request (HTTP {0}).")]
    Remote(u16),
    #[error("A previously saved Google Drive object is missing. Local writing was preserved.")]
    MissingObject,
    #[error("Google Drive returned an invalid or unsupported response.")]
    Protocol,
    #[error("A sync object exceeds the supported size limit.")]
    ObjectTooLarge,
    #[error("The sync data does not match its saved identity. Local writing was preserved.")]
    Integrity,
    #[error("The recovery key is invalid or does not unlock this sync collection.")]
    RecoveryKey,
    #[error("The secure local Google sign-in callback could not be opened.")]
    Loopback,
}

pub type Result<T> = std::result::Result<T, Error>;
