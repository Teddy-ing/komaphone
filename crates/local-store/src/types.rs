//! Portable local-draft requests. Native I/O is implemented separately.
use photocraft_doc::Document;
use photocraft_party::contracts::{EventId, ProjectId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LOCAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayKind {
    Baseline,
    Resolved,
    Checkpoint,
    Undo,
    Redo,
}

/// Parameters are resolved before the Actions journal can rewrite/coalesce them.
/// Every step also owns a lossless result checkpoint in the storage transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRecord {
    pub schema_version: u32,
    pub command: String,
    pub params: Value,
    pub elapsed_ms: u64,
    pub kind: ReplayKind,
}

#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub project: ProjectId,
    pub expected_revision: u64,
    pub request_id: EventId,
    pub title: String,
    pub document: Document,
    pub event: ReplayRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveAck {
    pub project: ProjectId,
    pub revision: u64,
    pub event_id: EventId,
    pub device_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftInfo {
    pub project: ProjectId,
    pub title: String,
    pub revision: u64,
    pub updated_at: String,
    pub replay_elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct LoadedDraft {
    pub document: Document,
    pub info: DraftInfo,
    pub head_revision: u64,
    pub recovery_warning: Option<String>,
}
