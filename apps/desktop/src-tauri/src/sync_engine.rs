//! Replayable immutable exchange. No network work runs on the timer's worker.
use std::collections::{BTreeMap, BTreeSet};

use rhythm_desktop_service::{SyncArchive, SyncImportReport, SyncObject};
use rhythm_sync::{
    ObjectStore, RemoteObject, VaultDescriptor, VaultKey, decrypt_object, encrypt_object,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const DESCRIPTOR_NAME: &str = "rhythm-vault-v1.json";

#[cfg(test)]
#[path = "sync_engine_tests.rs"]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingUpload {
    pub remote_id: String,
    pub name: String,
    pub ciphertext_hash: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncState {
    pub descriptor: VaultDescriptor,
    pub descriptor_id: String,
    pub key: VaultKey,
    pub device_id: String,
    pub seen: BTreeMap<String, RemoteObject>,
    pub pending: Option<PendingUpload>,
    pub last_success_at_utc_ms: Option<i64>,
}

pub struct LocalSnapshot {
    pub device_id: String,
    pub objects: Vec<SyncObject>,
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub reconnect_required: bool,
    pub retryable: bool,
}

impl From<rhythm_sync::Error> for Failure {
    fn from(error: rhythm_sync::Error) -> Self {
        Self {
            reconnect_required: matches!(error, rhythm_sync::Error::ReconnectRequired),
            retryable: matches!(error, rhythm_sync::Error::Retryable),
            message: error.to_string(),
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            reconnect_required: false,
            retryable: false,
        }
    }
}

type Result<T> = std::result::Result<T, Failure>;

pub trait Host {
    fn snapshot(&mut self) -> std::result::Result<LocalSnapshot, String>;
    fn import(
        &mut self,
        device_id: &str,
        archive: &SyncArchive,
    ) -> std::result::Result<SyncImportReport, String>;
    fn checkpoint(&mut self, state: &SyncState) -> std::result::Result<(), String>;
    fn save_pending(&mut self, remote_id: &str, bytes: &[u8]) -> std::result::Result<(), String>;
    fn read_pending(&mut self, remote_id: &str) -> std::result::Result<Vec<u8>, String>;
    fn remove_pending(&mut self, remote_id: &str);
    fn cancelled(&self) -> bool;
    fn history_changed(&self);
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn check(host: &impl Host) -> Result<()> {
    if host.cancelled() {
        Err("Sync was stopped. Local writing is safe."
            .to_string()
            .into())
    } else {
        Ok(())
    }
}

fn object_name(vault_id: &str, id: &str) -> String {
    format!("rhythm-v1-{vault_id}-{id}.json")
}

fn logical_id<'a>(name: &'a str, vault_id: &str) -> Option<&'a str> {
    let prefix = format!("rhythm-v1-{vault_id}-");
    let id = name.strip_prefix(&prefix)?.strip_suffix(".json")?;
    (id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(id)
}

pub fn descriptors(objects: &[RemoteObject]) -> std::result::Result<Option<&RemoteObject>, String> {
    let matches: Vec<_> = objects
        .iter()
        .filter(|object| object.name == DESCRIPTOR_NAME)
        .collect();
    if matches.len() > 1 {
        return Err("More than one Rhythm sync collection was found. Sync is paused; local writing is safe.".into());
    }
    Ok(matches.first().copied())
}

/// Preserve both provider identity and exact ciphertext before the first write.
pub fn stage_upload(
    provider: &mut impl ObjectStore,
    state: &mut SyncState,
    host: &mut impl Host,
    name: String,
    bytes: &[u8],
) -> Result<()> {
    check(host)?;
    let remote_id = provider.allocate_id()?;
    host.save_pending(&remote_id, bytes)?;
    state.pending = Some(PendingUpload {
        remote_id,
        name,
        ciphertext_hash: digest(bytes),
    });
    host.checkpoint(state)?;
    Ok(())
}

fn replay_upload(
    provider: &mut impl ObjectStore,
    state: &mut SyncState,
    host: &mut impl Host,
) -> Result<()> {
    let Some(pending) = state.pending.clone() else {
        return Ok(());
    };
    check(host)?;
    // In-memory staging can outlive a failed checkpoint. Never send until the
    // exact provider identity and payload digest have a durable recovery record.
    host.checkpoint(state)?;
    let bytes = host.read_pending(&pending.remote_id)?;
    if digest(&bytes) != pending.ciphertext_hash {
        return Err(rhythm_sync::Error::Integrity.into());
    }
    provider.create_immutable(&pending.remote_id, &pending.name, &bytes)?;
    check(host)?;
    state.seen.insert(
        pending.remote_id.clone(),
        RemoteObject {
            id: pending.remote_id.clone(),
            name: pending.name,
            size: Some(bytes.len() as u64),
            modified_time: None,
            md5_checksum: None,
        },
    );
    state.pending = None;
    host.checkpoint(state)?;
    host.remove_pending(&pending.remote_id);
    Ok(())
}

/// Returns false when a bounded batch remains, so the next pass runs promptly.
pub fn synchronize(
    provider: &mut impl ObjectStore,
    state: &mut SyncState,
    host: &mut impl Host,
) -> Result<bool> {
    check(host)?;
    // First enrollment can recover a descriptor creation whose reply was lost.
    if state
        .pending
        .as_ref()
        .is_some_and(|p| p.name == DESCRIPTOR_NAME)
    {
        replay_upload(provider, state, host)?;
    }
    let remote = provider.list_objects()?;
    let descriptor = descriptors(&remote)?.ok_or_else(|| Failure::from(
        "Rhythm's sync collection was removed from Drive. Sync is paused; local writing is safe.".to_string()))?;
    if descriptor.id != state.descriptor_id {
        return Err("The Drive sync collection changed. Reconnect only after checking the account and recovery passphrase.".to_string().into());
    }
    let bytes = provider.read_object(&descriptor.id)?;
    let current: VaultDescriptor =
        serde_json::from_slice(&bytes).map_err(|_| Failure::from(rhythm_sync::Error::Protocol))?;
    if current != state.descriptor || state.key.vault_id() != current.vault_id {
        return Err(rhythm_sync::Error::Integrity.into());
    }
    let present: BTreeSet<_> = remote.iter().map(|object| object.id.as_str()).collect();
    if state.seen.keys().any(|id| !present.contains(id.as_str())) {
        return Err("Previously synced data was removed from Drive. Local writing is safe. Sync is paused for recovery.".to_string().into());
    }
    for object in &remote {
        if state
            .seen
            .get(&object.id)
            .is_some_and(|old| old.name != object.name)
        {
            return Err("Previously synced data was renamed in Drive. Sync is paused; local writing is safe.".to_string().into());
        }
    }
    let local = host.snapshot()?;
    if local.device_id != state.device_id {
        // A restored database needs a fresh import pass, never stale delivery receipts.
        state.device_id = local.device_id;
        state.seen.clear();
        host.checkpoint(state)?;
    }
    if state.pending.is_some() {
        replay_upload(provider, state, host)?;
    }
    let mut known_names = BTreeSet::new();
    let mut operations = 0;
    for object in &remote {
        check(host)?;
        if object.name == DESCRIPTOR_NAME {
            continue;
        }
        let Some(id) = logical_id(&object.name, &state.descriptor.vault_id) else {
            if object.name.starts_with("rhythm-") {
                return Err("Drive contains an unsupported Rhythm sync format or another collection. Update Rhythm before syncing.".to_string().into());
            }
            continue;
        };
        // Our uploaded binary files have these Drive fields. Missing metadata
        // cannot establish that a previously validated object is unchanged.
        if object.size.is_none()
            || object.md5_checksum.as_ref().is_none_or(String::is_empty)
            || object.modified_time.as_ref().is_none_or(String::is_empty)
        {
            return Err("Drive returned incomplete metadata for synced history. Sync is paused; local writing is safe.".to_string().into());
        }
        known_names.insert(object.name.clone());
        if state.seen.get(&object.id) == Some(object) {
            continue;
        }
        if operations >= 100 {
            return Ok(false);
        }
        let encrypted = provider.read_object(&object.id)?;
        let plain = Zeroizing::new(decrypt_object(&state.key, id, &encrypted)?);
        if digest(&plain) != id {
            return Err(rhythm_sync::Error::Integrity.into());
        }
        let archive = SyncArchive::from_json(&plain).map_err(|_| Failure::from(
            "A Drive history record is invalid or requires a newer Rhythm version. Local writing is safe.".to_string()))?;
        check(host)?;
        let report = host.import(&state.device_id, &archive)?;
        // Mark only after the local transaction committed. A crash before this save
        // simply reapplies an idempotent import on the next pass.
        state.seen.insert(object.id.clone(), object.clone());
        host.checkpoint(state)?;
        if report.inserted_notes > 0 || report.inserted_revisions > 0 {
            host.history_changed();
        }
        operations += 1;
    }
    // Export again after merges to preserve newly combined heads, not stale views.
    let local = host.snapshot()?;
    if local.device_id != state.device_id {
        return Err(
            "The journal was restored during sync. Retry after restoration finishes."
                .to_string()
                .into(),
        );
    }
    for object in local.objects {
        check(host)?;
        let name = object_name(&state.descriptor.vault_id, &object.id);
        if known_names.contains(&name) || state.seen.values().any(|item| item.name == name) {
            continue;
        }
        if operations >= 100 {
            return Ok(false);
        }
        let bytes = Zeroizing::new(object.archive.to_json().map_err(|_| {
            Failure::from(
                "A local history record could not be prepared for sync. Local writing is safe."
                    .to_string(),
            )
        })?);
        if digest(&bytes) != object.id {
            return Err(rhythm_sync::Error::Integrity.into());
        }
        let encrypted = encrypt_object(&state.key, &object.id, &bytes)?;
        stage_upload(provider, state, host, name.clone(), &encrypted)?;
        replay_upload(provider, state, host)?;
        known_names.insert(name);
        operations += 1;
    }
    Ok(true)
}
