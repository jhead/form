//! Per-turn metrics and tool invocations.
//!
//! The stats engine (W3) aggregates these tables and never touches the entry payloads, so a
//! turn must be recorded even when it aborted or failed — an aborted run is a real datum on
//! the Home dashboard, not a gap.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::protocol::{ModelRef, RunOutcome, Usage};

use super::{new_id, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocationRecord {
    pub tool_name: String,
    pub started_at: i64,
    pub duration_ms: i64,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRecord {
    /// Caller-minted so the record can be assembled before the turn ends; use
    /// [`TurnRecord::new`] unless you already have one.
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub model: ModelRef,
    pub started_at: i64,
    pub ended_at: i64,
    /// Time to first token. `None` when the run failed before producing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i64>,
    pub duration_ms: i64,
    pub usage: Usage,
    pub outcome: RunOutcome,
    #[serde(default)]
    pub tools: Vec<ToolInvocationRecord>,
}

impl TurnRecord {
    pub fn new(session_id: String, run_id: String, model: ModelRef) -> Self {
        Self {
            id: new_id("trn"),
            session_id,
            run_id,
            model,
            started_at: 0,
            ended_at: 0,
            ttft_ms: None,
            duration_ms: 0,
            usage: Usage::default(),
            outcome: RunOutcome::Completed,
            tools: Vec::new(),
        }
    }
}

impl Store {
    /// Writes the turn and its tool invocations atomically; W3 can therefore join the two
    /// tables without ever seeing a turn whose tools are half-written.
    pub fn record_turn(&self, turn: TurnRecord) -> Result<()> {
        self.with_tx(|tx| insert_turn(tx, &turn))
    }

    pub fn count_turns(&self, session_id: &str) -> Result<u64> {
        self.with_conn(|conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM turns WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )?;
            Ok(n.max(0) as u64)
        })
    }

    pub fn count_tool_invocations(&self) -> Result<u64> {
        self.with_conn(|conn| {
            let n: i64 =
                conn.query_row("SELECT COUNT(*) FROM tool_invocations", [], |r| r.get(0))?;
            Ok(n.max(0) as u64)
        })
    }
}

pub(in crate::app) fn insert_turn(conn: &Connection, turn: &TurnRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO turns (id, session_id, run_id, provider_id, model_id, thinking_level,
                            started_at, ended_at, ttft_ms, duration_ms, input, output,
                            cache_read, cache_write, reasoning, total_tokens, cost_total,
                            outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18)",
        params![
            turn.id,
            turn.session_id,
            turn.run_id,
            turn.model.provider_id,
            turn.model.model_id,
            turn.model.thinking_level.as_str(),
            turn.started_at,
            turn.ended_at,
            turn.ttft_ms,
            turn.duration_ms,
            turn.usage.input as i64,
            turn.usage.output as i64,
            turn.usage.cache_read as i64,
            turn.usage.cache_write as i64,
            turn.usage.reasoning.map(|r| r as i64),
            turn.usage.total_tokens as i64,
            turn.usage.cost.total,
            outcome_str(turn.outcome),
        ],
    )?;
    for tool in &turn.tools {
        conn.execute(
            "INSERT INTO tool_invocations
               (id, session_id, turn_id, tool_name, started_at, duration_ms, is_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                new_id("tiv"),
                turn.session_id,
                turn.id,
                tool.tool_name,
                tool.started_at,
                tool.duration_ms,
                tool.is_error,
            ],
        )?;
    }
    Ok(())
}

fn outcome_str(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Completed => "completed",
        RunOutcome::Aborted => "aborted",
        RunOutcome::Failed => "failed",
    }
}
