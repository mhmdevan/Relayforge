use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("conflict: {0}")]
    Conflict(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainEvent {
    pub source: String,
    pub event_type: String,
    pub provider_event_id: Option<String>,
    pub occurred_at_unix: Option<i64>,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Received,
    Validated,
    Deduped,
    Queued,
    Processing,
    Succeeded,
    Failed,
    DeadLettered,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Received => "received",
            JobStatus::Validated => "validated",
            JobStatus::Deduped => "deduped",
            JobStatus::Queued => "queued",
            JobStatus::Processing => "processing",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::DeadLettered => "dead_lettered",
        }
    }
}

impl std::str::FromStr for JobStatus {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "received" => Ok(JobStatus::Received),
            "validated" => Ok(JobStatus::Validated),
            "deduped" => Ok(JobStatus::Deduped),
            "queued" => Ok(JobStatus::Queued),
            "processing" => Ok(JobStatus::Processing),
            "succeeded" => Ok(JobStatus::Succeeded),
            "failed" => Ok(JobStatus::Failed),
            "dead_lettered" => Ok(JobStatus::DeadLettered),
            _ => Err("unknown job status"),
        }
    }
}
