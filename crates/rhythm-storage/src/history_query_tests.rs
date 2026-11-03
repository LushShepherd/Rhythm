use super::*;
use crate::ReflectionInput;

fn utc(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .timestamp_millis()
}

fn period(
    store: &Store,
    id: &str,
    body: &str,
    created: i64,
    archived: i64,
    zone: Option<&str>,
    offset: Option<i32>,
) {
    store.connection.execute("INSERT INTO local_period_notes(period_id,kind,body,version,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,source_timezone,source_utc_offset_minutes) VALUES(?1,'focus',?2,1,?3,?3,?4,?5,?6)",
        params![id,body,created,archived,zone,offset]).unwrap();
}

#[test]
fn searches_the_complete_archive_and_treats_words_as_literal_unicode_substrings() {
    let store = Store::open_in_memory().unwrap();
    for index in 0..1_105 {
        period(
            &store,
            &format!("p-{index:04}"),
            if index == 3 {
                "ÉCOLE 中文 _ 100% ' OR 1=1"
            } else {
                "ordinary thoughts"
            },
            index,
            index + 10,
            None,
            None,
        );
    }
    let query = NoteHistoryQuery {
        text: "école 中文 _ 100%".into(),
        ..Default::default()
    };
    let page = store.query_note_history(&query, None, 10).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id, "p-0003");
    assert!(page.next_cursor.is_none());
    assert_eq!(
        store
            .query_note_history(
                &NoteHistoryQuery {
                    text: "' OR 1=1".into(),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        store
            .query_note_history(
                &NoteHistoryQuery {
                    text: "missingword".into(),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .entries
            .len(),
        0
    );
}

#[test]
fn sorting_uses_writing_instants_and_stable_keys_in_both_directions() {
    let store = Store::open_in_memory().unwrap();
    period(
        &store,
        "earlier-created-late-archive",
        "first",
        10,
        100,
        None,
        None,
    );
    period(&store, "tie-b", "third", 20, 21, None, None);
    period(&store, "tie-a", "second", 20, 22, None, None);
    let query = NoteHistoryQuery {
        order: "oldest".into(),
        ..Default::default()
    };
    let first = store.query_note_history(&query, None, 2).unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["earlier-created-late-archive", "tie-a"]
    );
    let last = store
        .query_note_history(&query, first.next_cursor.as_ref(), 2)
        .unwrap();
    assert_eq!(last.entries[0].id, "tie-b");
    assert!(last.next_cursor.is_none());
    let newest = store
        .query_note_history(&NoteHistoryQuery::default(), None, 10)
        .unwrap();
    assert_eq!(
        newest
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["tie-a", "tie-b", "earlier-created-late-archive"]
    );
}

#[test]
fn date_filters_convert_utc_without_moving_the_record_and_original_time_stays_fixed() {
    let store = Store::open_in_memory().unwrap();
    let created = utc("2026-11-02T07:30:00Z"); // Nov 1 23:30 in Los Angeles; Nov 2 afternoon in Shanghai.
    period(
        &store,
        "travelling-note",
        "same moment",
        created,
        created + 1000,
        Some("America/Los_Angeles"),
        Some(-480),
    );
    let la = NoteHistoryQuery {
        date_from: Some("2026-11-01".into()),
        date_to: Some("2026-11-01".into()),
        time_of_day: "night".into(),
        current_timezone: "America/Los_Angeles".into(),
        ..Default::default()
    };
    assert_eq!(
        store
            .query_note_history(&la, None, 10)
            .unwrap()
            .entries
            .len(),
        1
    );
    let shanghai = NoteHistoryQuery {
        date_from: Some("2026-11-02".into()),
        date_to: Some("2026-11-02".into()),
        time_of_day: "afternoon".into(),
        current_timezone: "Asia/Shanghai".into(),
        ..Default::default()
    };
    let shanghai_page = store.query_note_history(&shanghai, None, 10).unwrap();
    assert_eq!(shanghai_page.entries[0].created_at_utc_ms, created);
    let original = NoteHistoryQuery {
        time_basis: "original".into(),
        current_timezone: "Asia/Shanghai".into(),
        ..la.clone()
    };
    assert_eq!(
        store
            .query_note_history(&original, None, 10)
            .unwrap()
            .entries
            .len(),
        1
    );
    let wrong_day = NoteHistoryQuery {
        time_basis: "original".into(),
        ..shanghai
    };
    assert!(
        store
            .query_note_history(&wrong_day, None, 10)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn dst_repeated_hours_remain_distinct_and_unknown_origins_are_not_invented() {
    let store = Store::open_in_memory().unwrap();
    let first = utc("2026-11-01T08:30:00Z");
    let second = utc("2026-11-01T09:30:00Z");
    period(
        &store,
        "before-fallback",
        "first 1:30",
        first,
        first,
        Some("America/Los_Angeles"),
        Some(-420),
    );
    period(
        &store,
        "after-fallback",
        "second 1:30",
        second,
        second,
        Some("America/Los_Angeles"),
        Some(-480),
    );
    period(
        &store,
        "legacy",
        "unknown original zone",
        second + 1000,
        second + 1000,
        None,
        None,
    );
    let original = NoteHistoryQuery {
        time_basis: "original".into(),
        date_from: Some("2026-11-01".into()),
        date_to: Some("2026-11-01".into()),
        time_of_day: "night".into(),
        ..Default::default()
    };
    let page = store.query_note_history(&original, None, 10).unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["after-fallback", "before-fallback"]
    );
    assert_eq!(
        store
            .query_note_history(
                &NoteHistoryQuery {
                    time_basis: "original".into(),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .entries
            .len(),
        3
    );
    let current = NoteHistoryQuery {
        time_basis: "current".into(),
        current_timezone: "America/Los_Angeles".into(),
        ..original
    };
    assert_eq!(
        store
            .query_note_history(&current, None, 10)
            .unwrap()
            .entries
            .len(),
        3
    );
}

#[test]
fn query_and_history_changes_restart_paging_instead_of_mixing_results() {
    let store = Store::open_in_memory().unwrap();
    period(&store, "a", "alpha", 1, 1, None, None);
    period(&store, "b", "beta", 2, 2, None, None);
    let first = store
        .query_note_history(&NoteHistoryQuery::default(), None, 1)
        .unwrap();
    let filtered = store
        .query_note_history(
            &NoteHistoryQuery {
                text: "alpha".into(),
                ..Default::default()
            },
            first.next_cursor.as_ref(),
            1,
        )
        .unwrap();
    assert!(filtered.restarted);
    assert_eq!(filtered.entries[0].id, "a");
    period(&store, "c", "gamma", 3, 3, None, None);
    let changed = store
        .query_note_history(&NoteHistoryQuery::default(), first.next_cursor.as_ref(), 1)
        .unwrap();
    assert!(changed.restarted);
    assert_eq!(changed.entries[0].id, "c");
}

#[test]
fn searches_reflection_heads_older_versions_drafts_and_categories() {
    let mut store = Store::open_in_memory().unwrap();
    let draft = store
        .create_draft(ReflectionInput::thought("first version"), 1)
        .unwrap();
    let saved = store.finalize_draft(&draft.id, draft.version, 2).unwrap();
    let edit = store.edit_reflection(&saved.record_id, 3).unwrap();
    let edit = store
        .save_draft(&edit.id, edit.version, "current version", 4)
        .unwrap();
    store.finalize_draft(&edit.id, edit.version, 5).unwrap();
    store
        .create_draft(ReflectionInput::thought("unfinished writing"), 6)
        .unwrap();
    period(&store, "period", "focus writing", 7, 7, None, None);
    let older = store
        .query_note_history(
            &NoteHistoryQuery {
                source: "reflection_version".into(),
                ..Default::default()
            },
            None,
            10,
        )
        .unwrap();
    assert_eq!(older.entries.len(), 1);
    assert_eq!(older.entries[0].body, "first version");
    assert_eq!(
        store
            .query_note_history(
                &NoteHistoryQuery {
                    source: "reflection".into(),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .entries[0]
            .body,
        "current version"
    );
    assert_eq!(
        store
            .query_note_history(
                &NoteHistoryQuery {
                    source: "draft".into(),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .entries[0]
            .body,
        "unfinished writing"
    );
    assert_eq!(
        store
            .query_note_history(
                &NoteHistoryQuery {
                    kind: "focus".into(),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[test]
fn malformed_queries_and_cursors_do_not_read_a_partial_history() {
    let store = Store::open_in_memory().unwrap();
    for query in [
        NoteHistoryQuery {
            text: "x".repeat(1025),
            ..Default::default()
        },
        NoteHistoryQuery {
            text: "word ".repeat(33),
            ..Default::default()
        },
        NoteHistoryQuery {
            date_from: Some("2026-02-30".into()),
            ..Default::default()
        },
        NoteHistoryQuery {
            date_from: Some("2026-2-3".into()),
            ..Default::default()
        },
        NoteHistoryQuery {
            date_from: Some("2026-11-03".into()),
            date_to: Some("2026-11-02".into()),
            ..Default::default()
        },
        NoteHistoryQuery {
            current_timezone: "not/a/zone".into(),
            ..Default::default()
        },
        NoteHistoryQuery {
            order: "DESC;DROP TABLE records".into(),
            ..Default::default()
        },
    ] {
        assert!(store.query_note_history(&query, None, 10).is_err());
    }
    assert!(
        store
            .query_note_history(&NoteHistoryQuery::default(), None, 0)
            .is_err()
    );
    assert!(
        store
            .query_note_history(
                &NoteHistoryQuery::default(),
                Some(&NoteHistoryQueryCursor {
                    revision: "invalid".into(),
                    query_hash: "0".repeat(64),
                    at_utc_ms: 0,
                    key: "p".into()
                }),
                10
            )
            .is_err()
    );
}
