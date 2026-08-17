//! Store-level tests — spec 01 §7.
//!
//! Confinement lives in `workspace::tests` and title derivation in `title_tests`; everything
//! that needs a database is here.

use std::path::{Path, PathBuf};

use super::store::{AddAttachment, AttachmentSource, Store, StoreOptions, TurnRecord};
use super::{search::SearchScope, seed};
use crate::protocol::{
    EntryKind, Message, ModelRef, RunOutcome, SessionStatus, ThinkingLevel, UserMessage,
};

struct TempStore {
    dir: PathBuf,
    store: Store,
}

impl TempStore {
    fn new() -> Self {
        Self::with_options(StoreOptions::default())
    }

    fn with_options(options: StoreOptions) -> Self {
        let dir =
            std::env::temp_dir().join(format!("form-store-{}", uuid::Uuid::new_v4().simple()));
        let store = Store::open_with(&dir, options).expect("open store");
        Self { dir, store }
    }

    fn reopen(&self) -> Store {
        Store::open(&self.dir).expect("reopen store")
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn user(text: &str) -> EntryKind {
    EntryKind::Message {
        message: Message::User(UserMessage::text(text)),
    }
}

fn indexes(store: &Store, group: Option<&str>) -> Vec<(String, u32)> {
    store
        .list_sessions(true)
        .unwrap()
        .sessions
        .into_iter()
        .filter(|s| s.group_id.as_deref() == group)
        .map(|s| (s.title.clone(), s.index))
        .collect()
}

// ------------------------------------------------------------ lifecycle

#[test]
fn opens_empty_and_survives_a_reopen() {
    let t = TempStore::new();
    // Bump alongside a new migration — an accidental change here means a database in the
    // wild is being upgraded by something that was meant to be additive.
    assert_eq!(t.store.schema_version().unwrap(), 2);
    assert!(t.store.is_empty().unwrap());
    assert!(t.store.list_sessions(true).unwrap().sessions.is_empty());

    let s = t.store.create_session(None, None, None, None).unwrap();
    assert_eq!(s.title, super::UNTITLED);
    assert!(!s.title_is_custom);

    let reopened = t.reopen();
    assert!(!reopened.is_empty().unwrap());
    assert_eq!(reopened.get_summary(&s.id).unwrap().title, super::UNTITLED);
}

#[test]
fn a_stale_streaming_row_is_reset_to_idle_on_open() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();
    t.store.set_status(&s.id, SessionStatus::Streaming).unwrap();
    assert_eq!(
        t.store.get_summary(&s.id).unwrap().status,
        SessionStatus::Streaming
    );

    // A crash mid-run is indistinguishable from this: the row says streaming, no run exists.
    let reopened = t.reopen();
    assert_eq!(
        reopened.get_summary(&s.id).unwrap().status,
        SessionStatus::Idle
    );
}

#[test]
fn session_crud_round_trips() {
    let t = TempStore::new();
    let model = ModelRef {
        provider_id: "openai".into(),
        model_id: "gpt-5".into(),
        thinking_level: ThinkingLevel::Low,
    };
    let s = t
        .store
        .create_session(
            None,
            Some("Manual title".into()),
            Some("/tmp".into()),
            Some(model.clone()),
        )
        .unwrap();

    let back = t.store.get_summary(&s.id).unwrap();
    assert_eq!(back.title, "Manual title");
    assert!(back.title_is_custom, "an explicit title is a custom title");
    assert_eq!(back.model_ref, model);
    assert_eq!(back.workspace_root.as_deref(), Some("/tmp"));
    assert_eq!(back.status, SessionStatus::Idle);

    assert_eq!(
        t.store.add_tokens(&s.id, 1_500).unwrap().total_tokens,
        1_500
    );
    assert_eq!(t.store.add_tokens(&s.id, 500).unwrap().total_tokens, 2_000);
    assert!(t.store.set_pinned(&s.id, true).unwrap().pinned);
    assert!(t.store.set_archived(&s.id, true).unwrap().archived);

    // Archived sessions are hidden by default and returned on request.
    assert!(t.store.list_sessions(false).unwrap().sessions.is_empty());
    assert_eq!(t.store.list_sessions(true).unwrap().sessions.len(), 1);

    let renamed = t
        .store
        .set_session_model(&s.id, &super::default_model_ref())
        .unwrap();
    assert_eq!(renamed.model_ref, super::default_model_ref());

    t.store.delete_session(&s.id).unwrap();
    assert_eq!(
        t.store.get_summary(&s.id).unwrap_err().code(),
        "session_not_found"
    );
}

#[test]
fn missing_ids_report_their_own_error_codes() {
    let t = TempStore::new();
    assert_eq!(
        t.store.get_session("nope").unwrap_err().code(),
        "session_not_found"
    );
    assert_eq!(
        t.store.append_entry("nope", user("hi")).unwrap_err().code(),
        "session_not_found"
    );
    assert_eq!(
        t.store.rename_group("nope", "x").unwrap_err().code(),
        "group_not_found"
    );
    assert_eq!(
        t.store.get_attachment("nope").unwrap_err().code(),
        "attachment_not_found"
    );
    assert_eq!(
        t.store
            .create_session(Some("nope".into()), None, None, None)
            .unwrap_err()
            .code(),
        "group_not_found"
    );
}

// ------------------------------------------------------------ entries

#[test]
fn entries_append_in_sequence_with_parent_linkage() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();

    let a = t.store.append_entry(&s.id, user("first")).unwrap();
    let b = t.store.append_entry(&s.id, user("second")).unwrap();
    assert_eq!((a.seq, b.seq), (0, 1));
    assert_eq!(a.parent_id, None);
    assert_eq!(b.parent_id.as_deref(), Some(a.id.as_str()));

    let session = t.store.get_session(&s.id).unwrap();
    assert_eq!(session.entries.len(), 2);
    assert_eq!(session.summary.message_count, 2);
    assert!(session.summary.updated_at >= a.timestamp);
}

#[test]
fn replacing_an_entry_rewrites_its_payload_and_its_search_row() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();
    let entry = t.store.append_entry(&s.id, user("original text")).unwrap();

    assert_eq!(
        t.store
            .search("original", SearchScope::All, 10)
            .unwrap()
            .len(),
        1
    );

    let updated = crate::protocol::Entry {
        kind: user("rewritten body"),
        ..entry.clone()
    };
    t.store.replace_entry(&updated).unwrap();

    let entries = t.store.list_entries(&s.id).unwrap();
    assert_eq!(entries.len(), 1, "replace must not append");
    match &entries[0].kind {
        EntryKind::Message {
            message: Message::User(m),
        } => assert_eq!(m.content.to_text(), "rewritten body"),
        other => panic!("unexpected kind: {other:?}"),
    }
    assert!(t
        .store
        .search("original", SearchScope::All, 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        t.store
            .search("rewritten", SearchScope::All, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn truncate_after_drops_the_tail_and_its_search_rows() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();
    let keep = t.store.append_entry(&s.id, user("keep this one")).unwrap();
    t.store.append_entry(&s.id, user("discard alpha")).unwrap();
    t.store.append_entry(&s.id, user("discard beta")).unwrap();

    assert_eq!(t.store.truncate_after(&s.id, &keep.id).unwrap(), 2);
    assert_eq!(t.store.list_entries(&s.id).unwrap().len(), 1);
    assert_eq!(t.store.get_summary(&s.id).unwrap().message_count, 1);
    assert!(t
        .store
        .search("discard", SearchScope::All, 10)
        .unwrap()
        .is_empty());
}

// ------------------------------------------------------------ titles

#[test]
fn the_first_user_message_derives_a_title_and_a_rename_pins_it() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();

    let derived = t
        .store
        .maybe_derive_title(&s.id, "add a health check endpoint.")
        .unwrap()
        .expect("first message derives");
    assert_eq!(derived.title, "Add a health check endpoint");
    assert!(!derived.title_is_custom);

    let renamed = t.store.rename_session(&s.id, "  Health checks  ").unwrap();
    assert_eq!(renamed.title, "Health checks");
    assert!(renamed.title_is_custom);

    assert!(
        t.store
            .maybe_derive_title(&s.id, "something else entirely")
            .unwrap()
            .is_none(),
        "a custom title is never overwritten"
    );
    assert_eq!(t.store.get_summary(&s.id).unwrap().title, "Health checks");

    // The FTS mirror follows the rename.
    let hits = t
        .store
        .search("Health checks", SearchScope::All, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry_id, None);
}

#[test]
fn a_blank_message_derives_nothing() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();
    assert!(t
        .store
        .maybe_derive_title(&s.id, "   \n ")
        .unwrap()
        .is_none());
    assert_eq!(t.store.get_summary(&s.id).unwrap().title, super::UNTITLED);
}

// ------------------------------------------------------------ ordering

#[test]
fn new_sessions_land_at_the_top_and_indexes_stay_dense() {
    let t = TempStore::new();
    for title in ["one", "two", "three"] {
        t.store
            .create_session(None, Some(title.into()), None, None)
            .unwrap();
    }
    assert_eq!(
        indexes(&t.store, None),
        vec![("three".into(), 0), ("two".into(), 1), ("one".into(), 2)]
    );
}

#[test]
fn moving_within_a_group_renumbers_densely() {
    let t = TempStore::new();
    let mut ids = Vec::new();
    for title in ["a", "b", "c", "d"] {
        ids.push(
            t.store
                .create_session(None, Some(title.into()), None, None)
                .unwrap()
                .id,
        );
    }
    // Created newest-first, so the list is d, c, b, a.
    let d = &ids[3];
    t.store.move_session(d, None, 3).unwrap();
    assert_eq!(
        indexes(&t.store, None)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>(),
        vec!["c", "b", "a", "d"]
    );
    assert_eq!(
        indexes(&t.store, None)
            .into_iter()
            .map(|(_, i)| i)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "indexes are dense after a move"
    );

    // Back to the front.
    t.store.move_session(d, None, 0).unwrap();
    assert_eq!(
        indexes(&t.store, None)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>(),
        vec!["d", "c", "b", "a"]
    );
}

#[test]
fn moving_between_groups_renumbers_both_sides() {
    let t = TempStore::new();
    let work = t.store.create_group("Work").unwrap();
    let side = t.store.create_group("Side").unwrap();

    let mut work_ids = Vec::new();
    for title in ["w1", "w2", "w3"] {
        work_ids.push(
            t.store
                .create_session(Some(work.id.clone()), Some(title.into()), None, None)
                .unwrap()
                .id,
        );
    }
    t.store
        .create_session(Some(side.id.clone()), Some("s1".into()), None, None)
        .unwrap();

    // Move the middle of Work into Side at index 0.
    t.store
        .move_session(&work_ids[1], Some(&side.id), 0)
        .unwrap();

    assert_eq!(
        indexes(&t.store, Some(&work.id)),
        vec![("w3".into(), 0), ("w1".into(), 1)]
    );
    assert_eq!(
        indexes(&t.store, Some(&side.id)),
        vec![("w2".into(), 0), ("s1".into(), 1)]
    );

    // Out of a group entirely, with an out-of-range index that clamps.
    t.store.move_session(&work_ids[0], None, 99).unwrap();
    assert_eq!(indexes(&t.store, None), vec![("w1".into(), 0)]);
    assert_eq!(indexes(&t.store, Some(&work.id)), vec![("w3".into(), 0)]);
}

#[test]
fn deleting_a_session_closes_the_gap_it_leaves() {
    let t = TempStore::new();
    let mut ids = Vec::new();
    for title in ["a", "b", "c"] {
        ids.push(
            t.store
                .create_session(None, Some(title.into()), None, None)
                .unwrap()
                .id,
        );
    }
    // List order is c, b, a; delete b (index 1).
    t.store.delete_session(&ids[1]).unwrap();
    assert_eq!(
        indexes(&t.store, None),
        vec![("c".into(), 0), ("a".into(), 1)]
    );
}

#[test]
fn group_crud_reorders_and_orphans_rather_than_deletes() {
    let t = TempStore::new();
    let a = t.store.create_group("Alpha").unwrap();
    let b = t.store.create_group("Beta").unwrap();
    let c = t.store.create_group("Gamma").unwrap();
    assert_eq!((a.index, b.index, c.index), (0, 1, 2));

    let groups = t.store.reorder_group(&c.id, 0).unwrap();
    assert_eq!(
        groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
        vec!["Gamma", "Alpha", "Beta"]
    );

    let groups = t.store.rename_group(&a.id, "Alpha renamed").unwrap();
    assert!(groups.iter().any(|g| g.name == "Alpha renamed"));
    assert!(t.store.set_group_collapsed(&b.id, true).unwrap()[2].collapsed);

    let s = t
        .store
        .create_session(Some(b.id.clone()), Some("orphan".into()), None, None)
        .unwrap();
    let groups = t.store.delete_group(&b.id).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups.iter().map(|g| g.index).collect::<Vec<_>>(),
        vec![0, 1],
        "group indexes stay dense"
    );
    let orphan = t.store.get_summary(&s.id).unwrap();
    assert_eq!(orphan.group_id, None, "sessions survive their group");
    assert_eq!(orphan.index, 0);
}

// ------------------------------------------------------------ search

fn seeded_search_store() -> TempStore {
    let t = TempStore::new();
    let a = t
        .store
        .create_session(None, Some("Rate limiting middleware".into()), None, None)
        .unwrap();
    t.store
        .append_entry(
            &a.id,
            user("Add a token bucket rate limiter keyed by API key."),
        )
        .unwrap();
    t.store
        .append_entry(&a.id, user("Return a Retry-After header on rejection."))
        .unwrap();

    let b = t
        .store
        .create_session(None, Some("Dashboard performance".into()), None, None)
        .unwrap();
    t.store
        .append_entry(&b.id, user("The dashboard is slow for large accounts."))
        .unwrap();
    t
}

#[test]
fn search_ranks_titles_above_bodies_and_returns_explicit_ranges() {
    let t = seeded_search_store();
    let hits = t.store.search("limiting", SearchScope::All, 10).unwrap();
    assert!(!hits.is_empty());
    let top = &hits[0];
    assert_eq!(top.title, "Rate limiting middleware");
    assert_eq!(top.entry_id, None, "the title row outranks the body rows");

    // The snippet carries no markup; the ranges say where to highlight it.
    assert!(!top.snippet.contains('\u{e000}'));
    assert!(!top.snippet.contains('\u{e001}'));
    assert_eq!(top.highlights.len(), 1);
    let h = top.highlights[0];
    let utf16: Vec<u16> = top.snippet.encode_utf16().collect();
    let matched = String::from_utf16(&utf16[h.start as usize..(h.start + h.len) as usize]).unwrap();
    assert_eq!(matched.to_lowercase(), "limiting");
    assert!(top.score.is_finite());
}

#[test]
fn search_finds_message_bodies_and_reports_the_entry() {
    let t = seeded_search_store();
    let hits = t
        .store
        .search("token bucket", SearchScope::All, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].entry_id.is_some());
    assert_eq!(hits[0].title, "Rate limiting middleware");
    assert_eq!(hits[0].highlights.len(), 2, "one range per matched term");
}

#[test]
fn search_can_be_scoped_to_one_session() {
    let t = seeded_search_store();
    let ids: Vec<(String, String)> = t
        .store
        .list_sessions(true)
        .unwrap()
        .sessions
        .into_iter()
        .map(|s| (s.title, s.id))
        .collect();
    let rate = ids
        .iter()
        .find(|(t, _)| t.starts_with("Rate"))
        .unwrap()
        .1
        .clone();
    let dash = ids
        .iter()
        .find(|(t, _)| t.starts_with("Dash"))
        .unwrap()
        .1
        .clone();

    // Unscoped, "dashboard" reaches across sessions.
    let all = t.store.search("dashboard", SearchScope::All, 50).unwrap();
    assert!(all.iter().any(|h| h.session_id == dash));

    // Scoped to the other session, the same query finds nothing.
    assert!(t
        .store
        .search("dashboard", SearchScope::Session(rate.clone()), 50)
        .unwrap()
        .is_empty());

    let scoped = t
        .store
        .search("rate", SearchScope::Session(rate.clone()), 50)
        .unwrap();
    assert!(!scoped.is_empty());
    assert!(scoped.iter().all(|h| h.session_id == rate));
}

#[test]
fn search_is_prefix_matched_and_operator_safe() {
    let t = seeded_search_store();
    assert!(!t
        .store
        .search("limi", SearchScope::All, 10)
        .unwrap()
        .is_empty());
    // None of these are valid fts5 syntax on their own; none may raise.
    for q in ["\"", "AND", "foo:", "*", "NEAR(", "-", ""] {
        t.store
            .search(q, SearchScope::All, 10)
            .unwrap_or_else(|e| panic!("query {q:?} raised {e}"));
    }
}

#[test]
fn archived_sessions_drop_out_of_global_search_but_not_scoped_search() {
    let t = seeded_search_store();
    let target = t
        .store
        .list_sessions(true)
        .unwrap()
        .sessions
        .into_iter()
        .find(|s| s.title == "Rate limiting middleware")
        .unwrap();
    t.store.set_archived(&target.id, true).unwrap();

    assert!(t
        .store
        .search("limiting", SearchScope::All, 10)
        .unwrap()
        .is_empty());
    assert!(!t
        .store
        .search("limiting", SearchScope::Session(target.id.clone()), 10)
        .unwrap()
        .is_empty());
}

#[test]
fn deleting_a_session_removes_it_from_the_index() {
    let t = seeded_search_store();
    let target = t
        .store
        .list_sessions(true)
        .unwrap()
        .sessions
        .into_iter()
        .find(|s| s.title == "Rate limiting middleware")
        .unwrap();
    t.store.delete_session(&target.id).unwrap();
    assert!(t
        .store
        .search("limiting", SearchScope::All, 10)
        .unwrap()
        .is_empty());
    assert!(t
        .store
        .search("bucket", SearchScope::All, 10)
        .unwrap()
        .is_empty());
}

// ------------------------------------------------------------ branching

#[test]
fn branching_copies_the_prefix_and_records_where_it_came_from() {
    let t = TempStore::new();
    let s = t
        .store
        .create_session(None, Some("Original".into()), None, None)
        .unwrap();
    let e0 = t.store.append_entry(&s.id, user("first")).unwrap();
    let e1 = t.store.append_entry(&s.id, user("second")).unwrap();
    t.store.append_entry(&s.id, user("third")).unwrap();

    let branch = t.store.branch_from_message(&s.id, &e1.id).unwrap();
    assert_ne!(branch.id, s.id);
    assert_eq!(branch.title, "Original");
    assert_eq!(branch.message_count, 2);

    let entries = t.store.list_entries(&branch.id).unwrap();
    assert_eq!(entries.len(), 3, "two copies plus the branch marker");
    assert!(entries.iter().all(|e| e.session_id == branch.id));
    assert!(
        entries.iter().all(|e| e.id != e0.id && e.id != e1.id),
        "copies get fresh ids"
    );
    assert_eq!(entries[0].parent_id, None);
    assert_eq!(
        entries[1].parent_id.as_deref(),
        Some(entries[0].id.as_str())
    );
    match &entries[2].kind {
        EntryKind::BranchSummary { from_id, summary } => {
            assert_eq!(from_id, &e1.id);
            assert!(summary.contains("Original"));
        }
        other => panic!("expected a branch summary, got {other:?}"),
    }

    // The original is untouched and both are searchable.
    assert_eq!(t.store.list_entries(&s.id).unwrap().len(), 3);
    let hits = t.store.search("second", SearchScope::All, 10).unwrap();
    assert_eq!(hits.len(), 2, "the copy is indexed too");
}

#[test]
fn branching_from_an_unknown_entry_is_an_invalid_request() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();
    assert_eq!(
        t.store
            .branch_from_message(&s.id, "ent_nope")
            .unwrap_err()
            .code(),
        "invalid_request"
    );
}

// ------------------------------------------------------------ attachments

fn png_bytes(variant: u8) -> Vec<u8> {
    seed::png::encode_gradient(48, 32, variant)
}

#[test]
fn identical_content_is_stored_once_and_recorded_twice() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();
    let bytes = png_bytes(0);

    let a = t
        .store
        .add_attachment(AddAttachment {
            session_id: Some(s.id.clone()),
            source: AttachmentSource::Bytes(bytes.clone()),
            filename: "shot.png".into(),
            mime: "image/png".into(),
        })
        .unwrap();
    let b = t
        .store
        .add_attachment(AddAttachment {
            session_id: Some(s.id.clone()),
            source: AttachmentSource::Bytes(bytes.clone()),
            filename: "shot-copy.png".into(),
            mime: "image/png".into(),
        })
        .unwrap();

    assert_ne!(a.id, b.id);
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(a.path, b.path, "one blob, two records");
    assert_eq!(std::fs::read(&a.path).unwrap(), bytes);
    assert_eq!((a.width, a.height), (Some(48), Some(32)));
    assert_eq!(a.bytes, bytes.len() as u64);
    assert_eq!(t.store.list_attachments(&s.id).unwrap().len(), 2);

    // Removing one record keeps the shared blob alive.
    t.store.remove_attachment(&a.id).unwrap();
    assert!(Path::new(&b.path).exists());
    t.store.remove_attachment(&b.id).unwrap();
    assert!(
        !Path::new(&b.path).exists(),
        "the last record takes the blob"
    );
}

#[test]
fn different_content_hashes_differently() {
    let t = TempStore::new();
    let a = t
        .store
        .add_attachment(AddAttachment {
            session_id: None,
            source: AttachmentSource::Bytes(png_bytes(0)),
            filename: "a.png".into(),
            mime: "image/png".into(),
        })
        .unwrap();
    let b = t
        .store
        .add_attachment(AddAttachment {
            session_id: None,
            source: AttachmentSource::Bytes(png_bytes(2)),
            filename: "b.png".into(),
            mime: "image/png".into(),
        })
        .unwrap();
    assert_ne!(a.sha256, b.sha256);
    assert_ne!(a.path, b.path);
}

#[test]
fn oversized_and_unsupported_attachments_are_rejected_with_a_reason() {
    let t = TempStore::new();
    let too_big = vec![0u8; (super::MAX_ATTACHMENT_BYTES + 1) as usize];
    let err = t
        .store
        .add_attachment(AddAttachment {
            session_id: None,
            source: AttachmentSource::Bytes(too_big),
            filename: "huge.png".into(),
            mime: "image/png".into(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "attachment_rejected");
    assert!(err.to_string().contains("10 MB"), "{err}");

    let err = t
        .store
        .add_attachment(AddAttachment {
            session_id: None,
            source: AttachmentSource::Bytes(vec![1, 2, 3]),
            filename: "app".into(),
            mime: "application/x-mach-binary".into(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "attachment_rejected");

    let err = t
        .store
        .add_attachment(AddAttachment {
            session_id: None,
            source: AttachmentSource::Path("/definitely/not/here.png".into()),
            filename: "here.png".into(),
            mime: "image/png".into(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "attachment_rejected");
}

#[test]
fn an_attachment_can_come_from_a_file_and_carry_a_thumbnail_path() {
    let t = TempStore::new();
    let src = t.dir.join("source.png");
    std::fs::write(&src, png_bytes(1)).unwrap();

    let a = t
        .store
        .add_attachment(AddAttachment {
            session_id: None,
            source: AttachmentSource::Path(src.to_string_lossy().into_owned()),
            filename: "source.png".into(),
            mime: "image/png".into(),
        })
        .unwrap();
    assert_eq!(a.thumb_path, None);

    t.store.set_thumb_path(&a.id, "/tmp/thumb.png").unwrap();
    assert_eq!(
        t.store.get_attachment(&a.id).unwrap().thumb_path.as_deref(),
        Some("/tmp/thumb.png")
    );
}

// ------------------------------------------------------------ turns and roots

#[test]
fn turns_and_their_tool_invocations_are_recorded_together() {
    let t = TempStore::new();
    let s = t.store.create_session(None, None, None, None).unwrap();

    let mut record = TurnRecord::new(s.id.clone(), "run_1".into(), super::default_model_ref());
    record.started_at = 1_000;
    record.ended_at = 4_000;
    record.duration_ms = 3_000;
    record.ttft_ms = Some(420);
    record.outcome = RunOutcome::Aborted;
    record.usage.total_tokens = 900;
    record.tools = vec![
        super::ToolInvocationRecord {
            tool_name: "read".into(),
            started_at: 1_200,
            duration_ms: 40,
            is_error: false,
        },
        super::ToolInvocationRecord {
            tool_name: "bash".into(),
            started_at: 2_000,
            duration_ms: 900,
            is_error: true,
        },
    ];
    t.store.record_turn(record).unwrap();

    assert_eq!(t.store.count_turns(&s.id).unwrap(), 1);
    assert_eq!(t.store.count_tool_invocations().unwrap(), 2);

    // Deleting the session cascades to both tables the stats engine reads.
    t.store.delete_session(&s.id).unwrap();
    assert_eq!(t.store.count_turns(&s.id).unwrap(), 0);
    assert_eq!(t.store.count_tool_invocations().unwrap(), 0);
}

#[test]
fn recent_roots_are_deduped_and_ordered_by_last_use() {
    let t = TempStore::new();
    t.store.touch_recent_root("/a").unwrap();
    t.store.touch_recent_root("/b").unwrap();
    t.store.touch_recent_root("/a").unwrap();

    let roots = t.store.list_recent_roots().unwrap();
    assert_eq!(roots.len(), 2, "paths are deduped");
    assert_eq!(roots[0].path, "/a", "most recently used first");

    // Setting a session's root records it; clearing it leaves the session unconfined.
    let s = t.store.create_session(None, None, None, None).unwrap();
    let s = t
        .store
        .set_workspace_root(&s.id, Some("/c".into()))
        .unwrap();
    assert_eq!(s.workspace_root.as_deref(), Some("/c"));
    assert_eq!(t.store.list_recent_roots().unwrap()[0].path, "/c");

    let s = t.store.set_workspace_root(&s.id, None).unwrap();
    assert_eq!(s.workspace_root, None);
    assert_eq!(
        t.store.list_recent_roots().unwrap().len(),
        3,
        "history is kept"
    );
}

// ------------------------------------------------------------ seeding

fn seeded() -> TempStore {
    let t = TempStore::new();
    seed::seed(&t.store, seed::DEFAULT_SEED, ANCHOR).unwrap();
    t
}

/// A fixed anchor keeps the corpus byte-identical run to run.
const ANCHOR: i64 = 1_755_000_000_000;

#[test]
fn seeding_produces_the_corpus_spec_01_6_describes() {
    let t = seeded();
    let list = t.store.list_sessions(true).unwrap();
    assert_eq!(list.groups.len(), 3);
    assert!(
        (20..=30).contains(&list.sessions.len()),
        "~24 sessions, got {}",
        list.sessions.len()
    );

    // Every group is used, and some sessions are deliberately ungrouped.
    for group in &list.groups {
        assert!(
            list.sessions
                .iter()
                .any(|s| s.group_id.as_deref() == Some(&group.id)),
            "group {} is empty",
            group.name
        );
    }
    assert!(list.sessions.iter().any(|s| s.group_id.is_none()));

    // Indexes are dense within each bucket.
    for group in list.groups.iter().map(|g| Some(g.id.clone())).chain([None]) {
        let mut idx: Vec<u32> = list
            .sessions
            .iter()
            .filter(|s| s.group_id == group)
            .map(|s| s.index)
            .collect();
        idx.sort_unstable();
        assert_eq!(idx, (0..idx.len() as u32).collect::<Vec<_>>());
    }

    assert!(list.sessions.iter().any(|s| s.pinned));
    assert!(list.sessions.iter().any(|s| s.archived));
    assert!(list
        .sessions
        .iter()
        .any(|s| s.status == SessionStatus::Error));
    assert!(list.sessions.iter().any(|s| s.workspace_root.is_some()));
    assert!(!t.store.list_recent_roots().unwrap().is_empty());

    // Titles read like real sessions, not `Session 7`.
    assert!(list.sessions.iter().all(|s| s.title.len() > 8));
    assert!(list.sessions.iter().all(|s| s.message_count >= 4));
    assert!(list.sessions.iter().all(|s| s.total_tokens > 0));
}

#[test]
fn seeded_turns_span_the_window_with_several_models_and_all_three_outcomes() {
    let t = seeded();
    let stats: Vec<(String, String, String, i64, i64)> = t
        .store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT provider_id, model_id, outcome, started_at, total_tokens FROM turns",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();

    assert!(stats.len() > 100, "{} turns is too thin", stats.len());

    let models: std::collections::HashSet<_> =
        stats.iter().map(|s| (s.0.clone(), s.1.clone())).collect();
    assert!(models.len() >= 3, "several models: {models:?}");

    let outcomes: std::collections::HashSet<_> = stats.iter().map(|s| s.2.clone()).collect();
    for expected in ["completed", "aborted", "failed"] {
        assert!(
            outcomes.contains(expected),
            "no {expected} runs: {outcomes:?}"
        );
    }

    let oldest = stats.iter().map(|s| s.3).min().unwrap();
    let newest = stats.iter().map(|s| s.3).max().unwrap();
    let span_days = (newest - oldest) / 86_400_000;
    assert!(span_days > 90, "corpus spans only {span_days} days");
    assert!(
        newest <= ANCHOR + 86_400_000,
        "nothing may be in the future"
    );
    assert!(stats.iter().all(|s| s.4 >= 0));

    assert!(
        t.store.count_tool_invocations().unwrap() > 100,
        "a handful of tool calls per turn"
    );
}

/// Local day index of every turn, relative to the anchor's day: 0 is today, 1 yesterday.
fn active_days_back(t: &TempStore, anchor: i64) -> std::collections::BTreeSet<i64> {
    use chrono::{Local, TimeZone};
    let today = Local.timestamp_millis_opt(anchor).unwrap().date_naive();
    t.store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT started_at FROM turns")?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(rows)
        })
        .unwrap()
        .into_iter()
        .map(|ms| {
            let date = Local.timestamp_millis_opt(ms).unwrap().date_naive();
            (today - date).num_days()
        })
        .collect()
}

/// PRD acceptance criterion 3: the dashboard must be meaningful on first launch. That means
/// the `7d` tab — the default — has to be populated and the current-streak tile has to read
/// something other than zero, which needs an unbroken run of local days ending today.
#[test]
fn the_corpus_runs_up_to_today_with_an_unbroken_recent_streak() {
    let t = seeded();
    let days = active_days_back(&t, ANCHOR);

    assert!(days.contains(&0), "no activity today: {days:?}");
    assert!(days.contains(&1), "no activity yesterday: {days:?}");
    for day in 0..7 {
        assert!(days.contains(&day), "day -{day} is a hole in the streak");
    }
    // The 7d window should read as a week of work, not one bar.
    assert!(days.iter().filter(|d| **d < 7).count() >= 7);
}

#[test]
fn no_seeded_turn_is_dated_after_the_anchor() {
    let t = seeded();
    let latest: i64 = t
        .store
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT MAX(x) FROM (SELECT MAX(ended_at) AS x FROM turns
                                     UNION ALL SELECT MAX(timestamp) FROM entries)",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(
        latest <= ANCHOR,
        "corpus runs {}ms past the anchor",
        latest - ANCHOR
    );
    // ...and it really does reach today, rather than clearing the bar by stopping early.
    assert!(ANCHOR - latest < 4 * 3_600_000, "newest turn is stale");
}

/// The same guarantee, restated as the rule the stats engine actually applies, and checked
/// at every hour of the day. Local midnight is the awkward one: no time has elapsed today, so
/// there is genuinely no room for a session — the streak has to run from yesterday instead,
/// which is exactly what `stats::calc::streaks` allows.
#[test]
fn the_recency_guarantee_holds_whatever_the_anchor_falls_on() {
    for offset_hours in 0..24 {
        let anchor = ANCHOR - (ANCHOR % 86_400_000) + offset_hours * 3_600_000;
        let t = TempStore::new();
        seed::seed(&t.store, seed::DEFAULT_SEED, anchor).unwrap();
        let days = active_days_back(&t, anchor);

        let newest = *days.iter().next().expect("the corpus is never empty");
        assert!(
            newest <= 1,
            "anchor +{offset_hours}h: newest activity is {newest} days back, \
             which reads as a broken streak"
        );
        for day in newest..newest + 7 {
            assert!(
                days.contains(&day),
                "anchor +{offset_hours}h: day -{day} is a hole in {days:?}"
            );
        }
    }
}

#[test]
fn seeded_activity_has_a_diurnal_and_weekly_rhythm() {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let t = seeded();
    let starts: Vec<i64> = t
        .store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT started_at FROM turns")?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(rows)
        })
        .unwrap();

    let mut by_hour = [0u32; 24];
    let mut weekday = 0u32;
    let mut weekend = 0u32;
    for ms in &starts {
        let dt = Local.timestamp_millis_opt(*ms).unwrap();
        by_hour[dt.hour() as usize] += 1;
        if dt.weekday().num_days_from_monday() < 5 {
            weekday += 1;
        } else {
            weekend += 1;
        }
    }

    let working: u32 = by_hour[8..20].iter().sum();
    let night: u32 = by_hour[0..6].iter().sum();
    assert!(
        working > night * 4,
        "working hours {working} vs night {night} is not a diurnal shape"
    );
    assert!(
        weekday > weekend * 2,
        "weekday {weekday} vs weekend {weekend}"
    );
    assert!(weekend > 0, "weekends should be quiet, not empty");
}

#[test]
fn seeding_is_deterministic_for_a_fixed_seed_and_anchor() {
    let fingerprint = |t: &TempStore| -> Vec<(String, u64, u64, u32)> {
        let mut rows: Vec<_> = t
            .store
            .list_sessions(true)
            .unwrap()
            .sessions
            .into_iter()
            .map(|s| (s.title, s.message_count, s.total_tokens, s.index))
            .collect();
        rows.sort();
        rows
    };

    let a = seeded();
    let b = seeded();
    assert_eq!(fingerprint(&a), fingerprint(&b));

    let c = TempStore::new();
    seed::seed(&c.store, seed::DEFAULT_SEED + 1, ANCHOR).unwrap();
    assert_ne!(fingerprint(&a), fingerprint(&c), "a different seed differs");
}

#[test]
fn seeding_only_happens_on_an_empty_database() {
    let t = TempStore::with_options(StoreOptions {
        seed_mock_data: true,
        seed: seed::DEFAULT_SEED,
    });
    let first = t.store.list_sessions(true).unwrap().sessions.len();
    assert!(
        first > 0,
        "open_with(seed_mock_data) populates an empty store"
    );

    // Reopening with the same options must not double the corpus.
    let again = Store::open_with(
        &t.dir,
        StoreOptions {
            seed_mock_data: true,
            seed: seed::DEFAULT_SEED,
        },
    )
    .unwrap();
    assert_eq!(again.list_sessions(true).unwrap().sessions.len(), first);
    assert!(!seed::seed_if_empty(&again).unwrap());
}

#[test]
fn the_seeded_corpus_is_searchable_and_has_real_attachments() {
    let t = seeded();
    let hits = t
        .store
        .search("rate limiter", SearchScope::All, 10)
        .unwrap();
    assert!(!hits.is_empty(), "seeded transcripts are indexed");
    assert!(hits.iter().all(|h| !h.highlights.is_empty()));

    let attachments: Vec<String> = t
        .store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT path FROM attachments")?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .unwrap();
    assert!(attachments.len() >= 4, "{} attachments", attachments.len());
    for path in &attachments {
        let bytes = std::fs::read(path).expect("the blob exists on disk");
        assert!(
            bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "{path} is a real PNG"
        );
    }
}
