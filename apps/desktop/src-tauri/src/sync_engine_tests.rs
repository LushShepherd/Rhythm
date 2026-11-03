use super::*;
use rhythm_storage::{ReflectionInput, Store};
use std::{cell::Cell, sync::OnceLock};

fn vault() -> (VaultDescriptor, VaultKey) {
    static VAULT: OnceLock<(VaultDescriptor, VaultKey)> = OnceLock::new();
    VAULT
        .get_or_init(|| {
            VaultDescriptor::create("test recovery phrase for isolated profiles").unwrap()
        })
        .clone()
}

#[derive(Clone)]
struct StoredObject {
    metadata: RemoteObject,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct MemoryCloud {
    files: BTreeMap<String, StoredObject>,
    next_id: u64,
    writes: Vec<(String, String, Vec<u8>)>,
    lists: usize,
    lose_next_upload_response: bool,
}

impl MemoryCloud {
    fn insert(&mut self, id: &str, name: &str, bytes: &[u8]) {
        self.files.insert(
            id.into(),
            StoredObject {
                metadata: RemoteObject {
                    id: id.into(),
                    name: name.into(),
                    size: Some(bytes.len() as u64),
                    modified_time: Some("provider-generation".into()),
                    md5_checksum: Some(digest(bytes)),
                },
                bytes: bytes.to_vec(),
            },
        );
    }

    fn history_id(&self) -> String {
        self.files
            .values()
            .find(|file| file.metadata.name != DESCRIPTOR_NAME)
            .unwrap()
            .metadata
            .id
            .clone()
    }
}

impl ObjectStore for MemoryCloud {
    fn list_objects(&mut self) -> rhythm_sync::Result<Vec<RemoteObject>> {
        self.lists += 1;
        Ok(self
            .files
            .values()
            .map(|file| file.metadata.clone())
            .collect())
    }

    fn read_object(&mut self, id: &str) -> rhythm_sync::Result<Vec<u8>> {
        self.files
            .get(id)
            .map(|file| file.bytes.clone())
            .ok_or(rhythm_sync::Error::MissingObject)
    }

    fn allocate_id(&mut self) -> rhythm_sync::Result<String> {
        self.next_id += 1;
        Ok(format!("allocated-{}", self.next_id))
    }

    fn create_immutable(&mut self, id: &str, name: &str, bytes: &[u8]) -> rhythm_sync::Result<()> {
        self.writes.push((id.into(), name.into(), bytes.to_vec()));
        if let Some(existing) = self.files.get(id) {
            if existing.metadata.name != name || existing.bytes != bytes {
                return Err(rhythm_sync::Error::Integrity);
            }
        } else {
            self.insert(id, name, bytes);
        }
        if std::mem::take(&mut self.lose_next_upload_response) {
            return Err(rhythm_sync::Error::Retryable);
        }
        Ok(())
    }
}

struct Profile {
    store: Store,
    persisted: Option<SyncState>,
    pending_files: BTreeMap<String, Vec<u8>>,
    fail_checkpoint: bool,
    fail_save: bool,
    cancelled: bool,
    cancel_after_save: bool,
    history_events: Cell<usize>,
}

impl Profile {
    fn new() -> Self {
        Self::with_store(Store::open_in_memory().unwrap())
    }

    fn with_store(store: Store) -> Self {
        Self {
            store,
            persisted: None,
            pending_files: BTreeMap::new(),
            fail_checkpoint: false,
            fail_save: false,
            cancelled: false,
            cancel_after_save: false,
            history_events: Cell::new(0),
        }
    }

    fn write(&mut self, text: &str) -> String {
        let draft = self
            .store
            .create_draft(ReflectionInput::thought(text), 1)
            .unwrap();
        self.store
            .finalize_draft(&draft.id, draft.version, 2)
            .unwrap()
            .record_id
    }

    fn edit(&mut self, record: &str, text: &str) {
        let draft = self.store.edit_reflection(record, 3).unwrap();
        let draft = self
            .store
            .save_draft(&draft.id, draft.version, text, 4)
            .unwrap();
        self.store
            .finalize_draft(&draft.id, draft.version, 5)
            .unwrap();
    }

    fn state(&self) -> SyncState {
        let (descriptor, key) = vault();
        SyncState {
            descriptor,
            descriptor_id: "descriptor".into(),
            key,
            device_id: self.store.device_id().into(),
            seen: BTreeMap::new(),
            pending: None,
            last_success_at_utc_ms: None,
        }
    }
}

impl Host for Profile {
    fn snapshot(&mut self) -> std::result::Result<LocalSnapshot, String> {
        Ok(LocalSnapshot {
            device_id: self.store.device_id().into(),
            objects: self
                .store
                .export_sync_objects()
                .map_err(|error| error.to_string())?,
        })
    }

    fn import(
        &mut self,
        device_id: &str,
        archive: &SyncArchive,
    ) -> std::result::Result<SyncImportReport, String> {
        if device_id != self.store.device_id() {
            return Err("profile changed".into());
        }
        self.store
            .import_sync_archive(archive)
            .map_err(|error| error.to_string())
    }

    fn checkpoint(&mut self, state: &SyncState) -> std::result::Result<(), String> {
        if self.fail_checkpoint {
            return Err("simulated durable-state write failure".into());
        }
        self.persisted = Some(state.clone());
        Ok(())
    }

    fn save_pending(&mut self, id: &str, bytes: &[u8]) -> std::result::Result<(), String> {
        if self.fail_save {
            return Err("simulated pending-file write failure".into());
        }
        self.pending_files.insert(id.into(), bytes.to_vec());
        self.cancelled |= self.cancel_after_save;
        Ok(())
    }

    fn read_pending(&mut self, id: &str) -> std::result::Result<Vec<u8>, String> {
        self.pending_files
            .get(id)
            .cloned()
            .ok_or("pending bytes missing".into())
    }

    fn remove_pending(&mut self, id: &str) {
        self.pending_files.remove(id);
    }
    fn cancelled(&self) -> bool {
        self.cancelled
    }
    fn history_changed(&self) {
        self.history_events.set(self.history_events.get() + 1);
    }
}

fn cloud() -> MemoryCloud {
    let mut cloud = MemoryCloud::default();
    cloud.insert(
        "descriptor",
        DESCRIPTOR_NAME,
        &serde_json::to_vec(&vault().0).unwrap(),
    );
    cloud
}

#[test]
fn isolated_windows_profiles_converge_after_offline_edits_without_plaintext_cloud_data() {
    let directory = tempfile::tempdir().unwrap();
    let mut a = Profile::with_store(Store::open(directory.path().join("a.sqlite3")).unwrap());
    let mut b = Profile::with_store(Store::open(directory.path().join("b.sqlite3")).unwrap());
    let mut cloud = cloud();
    let mut a_state = a.state();
    let mut b_state = b.state();
    let record = a.write("private original words 简体 繁體");
    assert!(synchronize(&mut cloud, &mut a_state, &mut a).unwrap());
    assert!(synchronize(&mut cloud, &mut b_state, &mut b).unwrap());
    a.edit(&record, "private offline branch A");
    b.edit(&record, "private offline branch B");
    assert!(synchronize(&mut cloud, &mut a_state, &mut a).unwrap());
    assert!(synchronize(&mut cloud, &mut b_state, &mut b).unwrap());
    assert!(synchronize(&mut cloud, &mut a_state, &mut a).unwrap());
    assert_eq!(
        a.store.export_sync_objects().unwrap(),
        b.store.export_sync_objects().unwrap()
    );
    assert_eq!(
        a.store
            .get_reflection(&record)
            .unwrap()
            .unwrap()
            .heads
            .len(),
        2
    );
    assert!(b.history_events.get() > 0);
    for file in cloud.files.values() {
        let encrypted = String::from_utf8_lossy(&file.bytes);
        assert!(!encrypted.contains("private original words"));
        assert!(!encrypted.contains("private offline branch"));
        assert!(!encrypted.contains("test recovery phrase"));
    }
    let writes = cloud.writes.len();
    assert!(synchronize(&mut cloud, &mut a_state, &mut a).unwrap());
    assert!(synchronize(&mut cloud, &mut b_state, &mut b).unwrap());
    assert_eq!(
        cloud.writes.len(),
        writes,
        "converged profiles must not create endless new objects"
    );
}

#[test]
fn lost_upload_reply_replays_identical_ciphertext_and_allocated_identity_after_restart() {
    let mut profile = Profile::new();
    profile.write("words which survive the timeout");
    let mut state = profile.state();
    let mut cloud = cloud();
    cloud.lose_next_upload_response = true;
    let failure = synchronize(&mut cloud, &mut state, &mut profile).unwrap_err();
    assert!(failure.retryable);
    assert_eq!(cloud.writes.len(), 1);
    assert!(profile.persisted.as_ref().unwrap().pending.is_some());
    let mut recovered = profile.persisted.clone().unwrap();
    assert!(synchronize(&mut cloud, &mut recovered, &mut profile).unwrap());
    assert_eq!(cloud.writes.len(), 2);
    assert_eq!(cloud.writes[0], cloud.writes[1]);
    assert_eq!(
        cloud.files.len(),
        2,
        "one descriptor and one history object"
    );
    assert!(recovered.pending.is_none());
    assert!(profile.pending_files.is_empty());
}

#[test]
fn failed_local_durability_never_permits_network_upload_even_on_same_process_retry() {
    for fail_save in [false, true] {
        let mut profile = Profile::new();
        profile.write("stay local until durable");
        profile.fail_save = fail_save;
        profile.fail_checkpoint = !fail_save;
        let mut state = profile.state();
        let mut cloud = cloud();
        for _ in 0..2 {
            assert!(synchronize(&mut cloud, &mut state, &mut profile).is_err());
            assert!(
                cloud.writes.is_empty(),
                "a failed checkpoint must not be bypassed by a retry"
            );
        }
        assert_eq!(
            profile.store.export_sync_archive().unwrap().revisions.len(),
            1
        );
    }
}

#[test]
fn duplicate_drive_names_are_verified_by_remote_identity_and_merged_once() {
    let mut source = Profile::new();
    source.write("duplicate filename does not duplicate words");
    let mut source_state = source.state();
    let mut cloud = cloud();
    synchronize(&mut cloud, &mut source_state, &mut source).unwrap();
    let original = cloud.files.get(&cloud.history_id()).unwrap().clone();
    cloud.insert(
        "another-provider-id",
        &original.metadata.name,
        &original.bytes,
    );
    let mut target = Profile::new();
    let mut state = target.state();
    assert!(synchronize(&mut cloud, &mut state, &mut target).unwrap());
    assert_eq!(
        target.store.export_sync_archive().unwrap().revisions.len(),
        1
    );
    assert_eq!(target.history_events.get(), 1);
    assert_eq!(state.seen.len(), 2);
    // A duplicate filename with corrupted bytes must not hide behind the good copy.
    cloud.insert(
        "another-provider-id",
        &original.metadata.name,
        b"corrupt encrypted bytes",
    );
    assert!(synchronize(&mut cloud, &mut state, &mut target).is_err());
    assert_eq!(
        target.store.export_sync_archive().unwrap().revisions.len(),
        1
    );
}

#[test]
fn wrong_vault_corruption_and_changed_descriptor_never_import_words() {
    let mut source = Profile::new();
    source.write("authenticated private words");
    let mut source_state = source.state();
    let mut shared = cloud();
    synchronize(&mut shared, &mut source_state, &mut source).unwrap();
    let id = shared.history_id();
    let original = shared.files.get(&id).unwrap().clone();
    for variant in 0..3 {
        let mut cloud = cloud();
        let mut bytes = original.bytes.clone();
        if variant == 0 {
            bytes = b"not valid encrypted JSON".to_vec();
        } else if variant == 1 {
            let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            envelope["vault_id"] = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into();
            bytes = serde_json::to_vec(&envelope).unwrap();
        } else {
            let mut descriptor = vault().0;
            descriptor.salt = "unexpected replacement salt".into();
            cloud.insert(
                "descriptor",
                DESCRIPTOR_NAME,
                &serde_json::to_vec(&descriptor).unwrap(),
            );
        }
        cloud.insert(&id, &original.metadata.name, &bytes);
        let mut target = Profile::new();
        let mut state = target.state();
        assert!(synchronize(&mut cloud, &mut state, &mut target).is_err());
        assert!(
            target
                .store
                .export_sync_archive()
                .unwrap()
                .revisions
                .is_empty()
        );
        assert!(state.seen.is_empty());
        assert!(cloud.writes.is_empty());
    }
}

#[test]
fn removed_remote_history_pauses_without_erasing_or_overwriting_local_history() {
    let mut profile = Profile::new();
    profile.write("independent local copy survives remote removal");
    let mut state = profile.state();
    let mut cloud = cloud();
    synchronize(&mut cloud, &mut state, &mut profile).unwrap();
    let before = profile.store.export_sync_objects().unwrap();
    cloud.files.remove(&cloud.history_id());
    let writes = cloud.writes.len();
    let failure = synchronize(&mut cloud, &mut state, &mut profile).unwrap_err();
    assert!(failure.message.contains("removed"));
    assert_eq!(profile.store.export_sync_objects().unwrap(), before);
    assert_eq!(cloud.writes.len(), writes);
}

#[test]
fn renamed_known_remote_object_cannot_be_treated_as_an_unrelated_file() {
    let mut profile = Profile::new();
    profile.write("retain words when their cloud object is renamed");
    let mut state = profile.state();
    let mut cloud = cloud();
    synchronize(&mut cloud, &mut state, &mut profile).unwrap();
    let before = profile.store.export_sync_objects().unwrap();
    let id = cloud.history_id();
    cloud.files.get_mut(&id).unwrap().metadata.name = "unrelated-backup.txt".into();
    let writes = cloud.writes.len();
    let failure = synchronize(&mut cloud, &mut state, &mut profile).unwrap_err();
    assert!(failure.message.contains("renamed"));
    assert_eq!(profile.store.export_sync_objects().unwrap(), before);
    assert_eq!(cloud.writes.len(), writes);
}

#[test]
fn missing_cloud_metadata_cannot_create_an_unverifiable_synced_receipt() {
    let mut source = Profile::new();
    source.write("verified history needs complete provider metadata");
    let mut source_state = source.state();
    let mut shared = cloud();
    synchronize(&mut shared, &mut source_state, &mut source).unwrap();
    let id = shared.history_id();
    let original = shared.files.get(&id).unwrap().clone();
    for missing in 0..3 {
        let mut cloud = cloud();
        let mut incomplete = original.clone();
        match missing {
            0 => incomplete.metadata.size = None,
            1 => incomplete.metadata.md5_checksum = None,
            _ => incomplete.metadata.modified_time = None,
        }
        cloud.files.insert(id.clone(), incomplete.clone());
        let mut target = Profile::new();
        let mut state = target.state();
        assert!(synchronize(&mut cloud, &mut state, &mut target).is_err());
        assert!(state.seen.is_empty());
        assert!(
            target
                .store
                .export_sync_archive()
                .unwrap()
                .revisions
                .is_empty()
        );
        // Even a receipt from an older client must not bypass this check.
        state.seen.insert(id.clone(), incomplete.metadata);
        assert!(synchronize(&mut cloud, &mut state, &mut target).is_err());
        assert!(cloud.writes.is_empty());
    }
}

#[test]
fn cancellation_before_work_or_after_staging_does_not_upload_and_can_resume() {
    let mut profile = Profile::new();
    profile.write("cancelled sync retains this note");
    let mut state = profile.state();
    let mut cloud = cloud();
    profile.cancelled = true;
    assert!(synchronize(&mut cloud, &mut state, &mut profile).is_err());
    assert_eq!(cloud.lists, 0);
    profile.cancelled = false;
    profile.cancel_after_save = true;
    assert!(synchronize(&mut cloud, &mut state, &mut profile).is_err());
    assert!(cloud.writes.is_empty());
    assert!(state.pending.is_some());
    profile.cancelled = false;
    profile.cancel_after_save = false;
    assert!(synchronize(&mut cloud, &mut state, &mut profile).unwrap());
    assert_eq!(cloud.writes.len(), 1);
    assert_eq!(
        profile.store.export_sync_archive().unwrap().revisions.len(),
        1
    );
}

#[test]
fn restored_profile_reimports_remote_history_instead_of_trusting_old_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("earlier.sqlite3");
    let mut a = Profile::new();
    let record = a.write("initial words");
    a.store.backup_to(&backup).unwrap();
    let mut b = Profile::new();
    let mut a_state = a.state();
    let mut b_state = b.state();
    let mut cloud = cloud();
    synchronize(&mut cloud, &mut a_state, &mut a).unwrap();
    synchronize(&mut cloud, &mut b_state, &mut b).unwrap();
    b.edit(&record, "later remote words");
    synchronize(&mut cloud, &mut b_state, &mut b).unwrap();
    synchronize(&mut cloud, &mut a_state, &mut a).unwrap();
    a.store.restore_from(&backup).unwrap();
    a.edit(&record, "restored offline branch");
    assert_ne!(a_state.device_id, a.store.device_id());
    synchronize(&mut cloud, &mut a_state, &mut a).unwrap();
    synchronize(&mut cloud, &mut b_state, &mut b).unwrap();
    assert_eq!(a_state.device_id, a.store.device_id());
    assert_eq!(
        a.store
            .get_reflection(&record)
            .unwrap()
            .unwrap()
            .heads
            .len(),
        2
    );
    assert_eq!(
        a.store.export_sync_objects().unwrap(),
        b.store.export_sync_objects().unwrap()
    );
}
