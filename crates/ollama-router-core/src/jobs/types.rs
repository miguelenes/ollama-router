//! Job identity, kind, and per-target / job-level status.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Hyphenated UUID for a durable model operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(Uuid);

impl JobId {
    /// Fresh random v4 identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse a hyphenated or simple UUID.
    pub fn parse(raw: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(raw.trim())?))
    }

    /// Borrow the inner UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}

impl FromStr for JobId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Pull vs delete fan-out.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Pull,
    Delete,
    Provision,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Delete => "delete",
            Self::Provision => "provision",
        }
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "pull" => Ok(Self::Pull),
            "delete" => Ok(Self::Delete),
            "provision" => Ok(Self::Provision),
            _ => Err(()),
        }
    }
}

/// Job-level lifecycle (summarized from targets).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Success,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }

    /// Still in flight (not a terminal summarize).
    pub fn is_incomplete(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            _ => Err(()),
        }
    }
}

/// Per-target outcome. Skip/success/absent/deleted summarize to job success.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    Pending,
    Running,
    Success,
    Failed,
    /// Operator cancel of an incomplete target (failure-like for job summary).
    Cancelled,
    AlreadyPresent,
    AlreadyAbsent,
    SkippedUnhealthy,
    SkippedCapacity,
    SkippedRamPressure,
    SkippedDisk,
    Deleted,
}

impl TargetStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::AlreadyPresent => "already_present",
            Self::AlreadyAbsent => "already_absent",
            Self::SkippedUnhealthy => "skipped_unhealthy",
            Self::SkippedCapacity => "skipped_capacity",
            Self::SkippedRamPressure => "skipped_ram_pressure",
            Self::SkippedDisk => "skipped_disk",
            Self::Deleted => "deleted",
        }
    }

    pub fn is_incomplete(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }

    pub fn is_success_like(self) -> bool {
        matches!(
            self,
            Self::Success
                | Self::AlreadyPresent
                | Self::AlreadyAbsent
                | Self::SkippedUnhealthy
                | Self::SkippedCapacity
                | Self::SkippedRamPressure
                | Self::SkippedDisk
                | Self::Deleted
        )
    }
}

impl fmt::Display for TargetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TargetStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "success" => Self::Success,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "already_present" => Self::AlreadyPresent,
            "already_absent" => Self::AlreadyAbsent,
            "skipped_unhealthy" => Self::SkippedUnhealthy,
            "skipped_capacity" => Self::SkippedCapacity,
            "skipped_ram_pressure" => Self::SkippedRamPressure,
            "skipped_disk" => Self::SkippedDisk,
            "deleted" => Self::Deleted,
            // Additive forward-compat: old binary reading a newer status string.
            _ => Self::Failed,
        })
    }
}

/// One node/model pair inside a job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobTarget {
    pub node: String,
    pub model: String,
    pub status: TargetStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl JobTarget {
    pub fn new(node: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            model: model.into(),
            status: TargetStatus::Pending,
            detail: None,
        }
    }

    /// Persistence view: node/model/status only.
    pub fn stripped(&self) -> Self {
        Self {
            node: self.node.clone(),
            model: self.model.clone(),
            status: self.status,
            detail: None,
        }
    }
}

/// Durable model-operation snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    pub status: JobStatus,
    pub created_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    pub models: Vec<String>,
    pub nodes: Vec<String>,
    pub targets: BTreeMap<String, JobTarget>,
}

impl Job {
    /// Map key `node:model` (model may itself contain `:`).
    pub fn target_key(node: &str, model: &str) -> String {
        format!("{node}:{model}")
    }

    /// All skip/success/absent/deleted → success; any incomplete → running; else failed.
    pub fn summarize_status(&self) -> JobStatus {
        let statuses: Vec<TargetStatus> = self.targets.values().map(|t| t.status).collect();
        if statuses.is_empty() {
            return JobStatus::Success;
        }
        if statuses.iter().all(|s| s.is_success_like()) {
            return JobStatus::Success;
        }
        if statuses.iter().any(|s| s.is_incomplete()) {
            return JobStatus::Running;
        }
        JobStatus::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_disk_roundtrip_and_success_like() {
        let s = TargetStatus::SkippedDisk;
        assert_eq!(s.as_str(), "skipped_disk");
        assert_eq!(s.to_string(), "skipped_disk");
        assert_eq!("skipped_disk".parse::<TargetStatus>(), Ok(s));
        assert!(s.is_success_like());
        assert!(!s.is_incomplete());
    }

    #[test]
    fn job_kind_provision_roundtrip() {
        assert_eq!(JobKind::Provision.as_str(), "provision");
        assert_eq!("provision".parse::<JobKind>(), Ok(JobKind::Provision));
        assert!("nope".parse::<JobKind>().is_err());
    }
}
