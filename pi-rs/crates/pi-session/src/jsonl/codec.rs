//! JSONL v4 line codec. Port of `harness/session/jsonl/codec.ts`.
//!
//! **This module defines the on-disk wire format and must stay byte-compatible
//! with the TypeScript implementation.** Every line is one JSON object plus a
//! trailing `\n`:
//!
//! ```text
//! line 1  {"kind":"header","version":4,"id":…,"createdAt":…,"cwd":…}
//! line n  {"kind":"entry","lane":"main","type":…,"id":…,…,"parentId":…,"seq":n,"timestamp":…}
//!         {"kind":"record","type":…,"id":…,"lane":…,…,"seq":n,"timestamp":…}
//!         {"kind":"lane","seq":n,"lane":…,"leafId":…}
//!         {"kind":"fact","seq":n,"fact":"name","name":…}
//!         {"kind":"fact","seq":n,"fact":"label","targetId":…,"label":…}
//! ```
//!
//! Upstream produces these with `JSON.stringify`, which drops `undefined`
//! values and preserves object insertion order. This port relies on
//! `serde_json`'s `preserve_order` feature plus the canonical key order fixed
//! in [`crate::types`] to reproduce both properties exactly.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{JsonlDecodeError, SessionError};
use crate::state::SessionMutation;
use crate::types::{
    decode_new_record, decode_provisioned_entry, Entry, LaneRecord, SessionMetadata,
};

/// The first line of every v4 session file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlV4Header {
    /// Always `"header"`.
    pub kind: String,
    /// Always `4`.
    pub version: u8,
    pub id: String,
    pub created_at: i64,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Preserved only when a v3 parent path could not be resolved to a session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_parent_session_path: Option<String>,
    /// Opaque application-owned metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl JsonlV4Header {
    pub fn new(id: impl Into<String>, created_at: i64, cwd: impl Into<String>) -> Self {
        Self {
            kind: "header".into(),
            version: 4,
            id: id.into(),
            created_at,
            cwd: cwd.into(),
            parent_session_id: None,
            legacy_parent_session_path: None,
            metadata: None,
        }
    }
}

fn parse_object(line: &str) -> Result<Map<String, Value>, JsonlDecodeError> {
    let value: Value =
        serde_json::from_str(line).map_err(|_| JsonlDecodeError::syntax("is not valid JSON"))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(JsonlDecodeError::schema("is not a JSON object")),
    }
}

fn require_string(
    map: &Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<String, JsonlDecodeError> {
    match map.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(JsonlDecodeError::schema(format!("has invalid {field}"))),
    }
}

fn optional_string(
    map: &Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<String>, JsonlDecodeError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(JsonlDecodeError::schema(format!("has invalid {field}"))),
    }
}

/// `seq` must be a safe positive integer.
fn require_sequence(map: &Map<String, Value>) -> Result<i64, JsonlDecodeError> {
    match map.get("seq").and_then(Value::as_i64) {
        Some(seq) if seq > 0 => Ok(seq),
        _ => Err(JsonlDecodeError::schema("has invalid seq")),
    }
}

/// `timestamp` must be a safe non-negative integer.
fn require_timestamp(map: &Map<String, Value>) -> Result<i64, JsonlDecodeError> {
    match map.get("timestamp").and_then(Value::as_i64) {
        Some(timestamp) if timestamp >= 0 => Ok(timestamp),
        _ => Err(JsonlDecodeError::schema("has invalid timestamp")),
    }
}

fn require_nullable_id(
    map: &Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<String>, JsonlDecodeError> {
    match map.get(key) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) => Ok(None),
        _ => Err(JsonlDecodeError::schema(format!("has invalid {field}"))),
    }
}

pub fn parse_header(line: &str) -> Result<JsonlV4Header, JsonlDecodeError> {
    let map = parse_object(line)?;
    if map.get("kind").and_then(Value::as_str) != Some("header") {
        return Err(JsonlDecodeError::schema("is not a header"));
    }
    if map.get("version").and_then(Value::as_u64) != Some(4) {
        return Err(JsonlDecodeError::schema("has unsupported session version"));
    }
    let parent_session_id = optional_string(&map, "parentSessionId", "parentSessionId")?;
    let legacy_parent_session_path =
        optional_string(&map, "legacyParentSessionPath", "legacyParentSessionPath")?;
    if parent_session_id.is_some() && legacy_parent_session_path.is_some() {
        return Err(JsonlDecodeError::schema(
            "has both parentSessionId and legacyParentSessionPath",
        ));
    }
    let metadata = match map.get("metadata") {
        None => None,
        Some(Value::Object(metadata)) => Some(metadata.clone()),
        Some(_) => return Err(JsonlDecodeError::schema("has invalid metadata")),
    };
    let created_at = match map.get("createdAt").and_then(Value::as_i64) {
        Some(created_at) if created_at >= 0 => created_at,
        _ => return Err(JsonlDecodeError::schema("has invalid timestamp")),
    };
    Ok(JsonlV4Header {
        kind: "header".into(),
        version: 4,
        id: require_string(&map, "id", "id")?,
        created_at,
        cwd: require_string(&map, "cwd", "cwd")?,
        parent_session_id,
        legacy_parent_session_path,
        metadata,
    })
}

pub fn encode_header(header: &JsonlV4Header) -> String {
    let mut line = serde_json::to_string(header).expect("header is serializable");
    line.push('\n');
    line
}

/// Project a header plus filesystem facts into backend metadata.
pub fn metadata_from_header(
    header: &JsonlV4Header,
    path: &str,
    modified_at: i64,
) -> SessionMetadata {
    let mut metadata = SessionMetadata::new(header.id.clone(), header.created_at);
    metadata.parent_session_id = header.parent_session_id.clone();
    metadata.set("cwd", Value::String(header.cwd.clone()));
    metadata.set("path", Value::String(path.to_string()));
    metadata.set("modifiedAt", Value::from(modified_at));
    metadata.set("sourceFormat", Value::from(4));
    if let Some(legacy) = &header.legacy_parent_session_path {
        metadata.set("legacyParentSessionPath", Value::String(legacy.clone()));
    }
    if let Some(extra) = &header.metadata {
        metadata.set("metadata", Value::Object(extra.clone()));
    }
    metadata
}

fn schema<T>(result: Result<T, String>) -> Result<T, JsonlDecodeError> {
    result.map_err(JsonlDecodeError::schema)
}

fn parse_entry_mutation(
    map: Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    let lane = optional_string(&map, "lane", "lane")?;
    // Validate the envelope before the payload so upstream's error messages and
    // ordering are preserved.
    require_string(&map, "id", "id")?;
    let type_name = require_string(&map, "type", "entry type")?;
    if crate::types::EntryType::from_str_opt(&type_name).is_none() {
        return Err(JsonlDecodeError::schema(format!(
            "has unknown entry type {type_name}"
        )));
    }
    let parent_id = require_nullable_id(&map, "parentId", "parentId")?;
    let timestamp = require_timestamp(&map)?;
    let provisioned = schema(decode_provisioned_entry(map))?;
    Ok(SessionMutation::Entry {
        lane,
        entry: Entry {
            id: provisioned.id,
            seq,
            parent_id,
            timestamp,
            payload: provisioned.payload,
            extra: provisioned.extra,
        },
    })
}

fn parse_record_mutation(
    map: Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    require_string(&map, "id", "id")?;
    require_string(&map, "lane", "lane")?;
    let type_name = require_string(&map, "type", "record type")?;
    if crate::types::RecordType::from_str_opt(&type_name).is_none() {
        return Err(JsonlDecodeError::schema(format!(
            "has unknown record type {type_name}"
        )));
    }
    let timestamp = require_timestamp(&map)?;
    if type_name == "operation_started" {
        let intent = match map.get("intent") {
            Some(Value::Object(intent)) => intent,
            _ => return Err(JsonlDecodeError::schema("has invalid intent")),
        };
        let kind = match intent.get("kind") {
            Some(Value::String(kind)) => kind.clone(),
            _ => return Err(JsonlDecodeError::schema("has invalid operation kind")),
        };
        if !matches!(kind.as_str(), "run" | "compaction" | "navigation") {
            return Err(JsonlDecodeError::schema(format!(
                "has unknown operation kind {kind}"
            )));
        }
    }
    if type_name == "operation_finished" {
        require_string(&map, "runId", "runId")?;
    }
    let new_record = schema(decode_new_record(map))?;
    Ok(SessionMutation::Record {
        record: LaneRecord {
            id: new_record.id,
            seq,
            lane: new_record.lane,
            timestamp,
            payload: new_record.payload,
            extra: new_record.extra,
        },
    })
}

fn parse_lane_mutation(
    map: Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    Ok(SessionMutation::Lane {
        seq,
        lane: require_string(&map, "lane", "lane")?,
        leaf_id: require_nullable_id(&map, "leafId", "leafId")?,
    })
}

fn parse_fact_mutation(
    map: Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    match map.get("fact").and_then(Value::as_str) {
        Some("name") => Ok(SessionMutation::Name {
            seq,
            name: optional_string(&map, "name", "name")?,
        }),
        Some("label") => Ok(SessionMutation::Label {
            seq,
            target_id: require_string(&map, "targetId", "targetId")?,
            label: optional_string(&map, "label", "label")?,
        }),
        _ => Err(JsonlDecodeError::schema("has unknown fact type")),
    }
}

pub fn parse_mutation(line: &str) -> Result<SessionMutation, JsonlDecodeError> {
    let map = parse_object(line)?;
    let seq = require_sequence(&map)?;
    match map.get("kind").and_then(Value::as_str) {
        Some("entry") => parse_entry_mutation(map, seq),
        Some("record") => parse_record_mutation(map, seq),
        Some("lane") => parse_lane_mutation(map, seq),
        Some("fact") => parse_fact_mutation(map, seq),
        _ => Err(JsonlDecodeError::schema("has unknown mutation kind")),
    }
}

pub fn encode_mutation(mutation: &SessionMutation) -> Result<String, SessionError> {
    let map = mutation_to_map(mutation)?;
    let mut line = serde_json::to_string(&map).map_err(|error| {
        SessionError::storage(format!("Failed to encode session mutation: {error}"))
    })?;
    line.push('\n');
    Ok(line)
}

fn mutation_to_map(mutation: &SessionMutation) -> Result<Map<String, Value>, SessionError> {
    let encode = |error: serde_json::Error| {
        SessionError::storage(format!("Failed to encode session mutation: {error}"))
    };
    let mut map = Map::new();
    match mutation {
        SessionMutation::Entry { lane, entry } => {
            map.insert("kind".into(), Value::String("entry".into()));
            if let Some(lane) = lane {
                map.insert("lane".into(), Value::String(lane.clone()));
            }
            map.extend(entry.to_map().map_err(encode)?);
        }
        SessionMutation::Record { record } => {
            map.insert("kind".into(), Value::String("record".into()));
            map.extend(record.to_map().map_err(encode)?);
        }
        SessionMutation::Lane { seq, lane, leaf_id } => {
            map.insert("kind".into(), Value::String("lane".into()));
            map.insert("seq".into(), Value::from(*seq));
            map.insert("lane".into(), Value::String(lane.clone()));
            map.insert(
                "leafId".into(),
                leaf_id.clone().map_or(Value::Null, Value::String),
            );
        }
        SessionMutation::Name { seq, name } => {
            map.insert("kind".into(), Value::String("fact".into()));
            map.insert("seq".into(), Value::from(*seq));
            map.insert("fact".into(), Value::String("name".into()));
            // `JSON.stringify` drops `name: undefined`, so a cleared name is an
            // absent key — not `null`.
            if let Some(name) = name {
                map.insert("name".into(), Value::String(name.clone()));
            }
        }
        SessionMutation::Label {
            seq,
            target_id,
            label,
        } => {
            map.insert("kind".into(), Value::String("fact".into()));
            map.insert("seq".into(), Value::from(*seq));
            map.insert("fact".into(), Value::String("label".into()));
            map.insert("targetId".into(), Value::String(target_id.clone()));
            if let Some(label) = label {
                map.insert("label".into(), Value::String(label.clone()));
            }
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::JsonlDecodeErrorKind;
    use crate::types::{
        CustomEntry, EntryPayload, OperationIntent, OperationStartedRecord, RecordPayload,
        RunIntent,
    };

    fn expect_header_round_trip(header: JsonlV4Header) {
        let encoded = encode_header(&header);
        assert!(encoded.ends_with('\n'));
        assert_eq!(parse_header(encoded.trim_end()).unwrap(), header);
    }

    fn expect_mutation_round_trip(mutation: SessionMutation) {
        let encoded = encode_mutation(&mutation).unwrap();
        assert!(encoded.ends_with('\n'));
        assert_eq!(parse_mutation(encoded.trim_end()).unwrap(), mutation);
    }

    #[test]
    fn round_trips_every_header_field_with_a_resolved_parent() {
        let mut header = JsonlV4Header::new("session", 1_700_000_000_000, "/workspace/project");
        header.parent_session_id = Some("parent".into());
        header.metadata = Some(
            serde_json::json!({ "owner": "agent", "nested": { "enabled": true }, "values": [1, null, "two"] })
                .as_object()
                .unwrap()
                .clone(),
        );
        expect_header_round_trip(header);
    }

    #[test]
    fn round_trips_an_unresolved_legacy_parent_path() {
        let mut header =
            JsonlV4Header::new("legacy-child", 1_700_000_000_001, "/workspace/project");
        header.legacy_parent_session_path = Some("/sessions/missing-parent.jsonl".into());
        expect_header_round_trip(header);
    }

    #[test]
    fn projects_header_and_filesystem_fields_into_metadata() {
        let mut header = JsonlV4Header::new("session", 1_700_000_000_000, "/workspace/project");
        header.legacy_parent_session_path = Some("/sessions/missing-parent.jsonl".into());
        header.metadata = Some(
            serde_json::json!({ "owner": "agent" })
                .as_object()
                .unwrap()
                .clone(),
        );
        let metadata = metadata_from_header(&header, "/sessions/session.jsonl", 1_700_000_000_100);
        assert_eq!(
            serde_json::to_value(&metadata).unwrap(),
            serde_json::json!({
                "id": "session",
                "createdAt": 1_700_000_000_000i64,
                "cwd": "/workspace/project",
                "path": "/sessions/session.jsonl",
                "modifiedAt": 1_700_000_000_100i64,
                "sourceFormat": 4,
                "legacyParentSessionPath": "/sessions/missing-parent.jsonl",
                "metadata": { "owner": "agent" },
            })
        );
    }

    #[test]
    fn returns_syntax_and_schema_errors() {
        assert_eq!(
            parse_mutation("{").unwrap_err().kind,
            JsonlDecodeErrorKind::Syntax
        );
        assert_eq!(
            parse_mutation(r#"{"kind":"unknown","seq":1}"#)
                .unwrap_err()
                .kind,
            JsonlDecodeErrorKind::Schema
        );
    }

    #[test]
    fn round_trips_a_lane_bound_entry_line() {
        expect_mutation_round_trip(SessionMutation::Entry {
            lane: Some("main".into()),
            entry: Entry {
                id: "entry-1".into(),
                seq: 1,
                parent_id: None,
                timestamp: 100,
                payload: EntryPayload::Custom(CustomEntry {
                    custom_type: "note".into(),
                    data: Some(serde_json::json!({ "text": "hello" })),
                }),
                extra: Map::new(),
            },
        });
    }

    #[test]
    fn round_trips_an_imported_entry_line_without_a_lane() {
        expect_mutation_round_trip(SessionMutation::Entry {
            lane: None,
            entry: Entry {
                id: "entry-1".into(),
                seq: 1,
                parent_id: None,
                timestamp: 100,
                payload: EntryPayload::Custom(CustomEntry {
                    custom_type: "note".into(),
                    data: None,
                }),
                extra: Map::new(),
            },
        });
    }

    #[test]
    fn round_trips_a_record_line() {
        expect_mutation_round_trip(SessionMutation::Record {
            record: LaneRecord {
                id: "run-1".into(),
                seq: 1,
                lane: "main".into(),
                timestamp: 100,
                payload: RecordPayload::OperationStarted(OperationStartedRecord {
                    source_leaf_id: None,
                    intent: OperationIntent::Run(RunIntent {
                        original_prompt: vec![],
                        initial_messages: vec![],
                        system_prompt_override: None,
                        resume_data: None,
                    }),
                }),
                extra: Map::new(),
            },
        });
    }

    #[test]
    fn round_trips_a_lane_line() {
        expect_mutation_round_trip(SessionMutation::Lane {
            seq: 1,
            lane: "thread".into(),
            leaf_id: Some("entry-1".into()),
        });
    }

    #[test]
    fn round_trips_fact_lines_including_cleared_values() {
        expect_mutation_round_trip(SessionMutation::Name {
            seq: 1,
            name: Some("Example".into()),
        });
        expect_mutation_round_trip(SessionMutation::Name { seq: 2, name: None });
        expect_mutation_round_trip(SessionMutation::Label {
            seq: 3,
            target_id: "entry-1".into(),
            label: Some("checkpoint".into()),
        });
    }

    #[test]
    fn rejects_malformed_lines() {
        for line in [
            r#"{"kind":"entry","type":"custom","id":"entry","parentId":null,"seq":1,"timestamp":1}"#,
            r#"{"kind":"record","type":"operation_started","id":"run","lane":"main","seq":1,"timestamp":1,"sourceLeafId":null}"#,
            r#"{"kind":"record","type":"operation_finished","id":"finish","lane":"main","seq":1,"timestamp":1,"outcome":"completed"}"#,
        ] {
            assert!(
                parse_mutation(line).is_err(),
                "expected rejection of {line}"
            );
        }
    }

    #[test]
    fn cleared_name_is_an_absent_key_not_null() {
        let encoded = encode_mutation(&SessionMutation::Name { seq: 2, name: None }).unwrap();
        assert_eq!(encoded, "{\"kind\":\"fact\",\"seq\":2,\"fact\":\"name\"}\n");
    }
}
