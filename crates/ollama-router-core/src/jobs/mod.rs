//! Placement-aware pull/delete with SQLite WAL persistence.
//!
//! Store path (default): `/var/lib/ollama-router/model-operations.sqlite3`.
//! No upstream bodies or per-target `detail` strings are persisted.

use std::future::Future;
use std::pin::Pin;

mod orchestrator;
mod store;
mod types;

pub use orchestrator::PullOrchestrator;
pub use store::{JobStore, StoreError};
pub use types::{Job, JobId, JobKind, JobStatus, JobTarget, TargetStatus};

/// Terminal job hook. Implemented in the binary (`Metrics`).
pub trait JobObserver: Send + Sync {
    fn job_terminal(&self, kind: JobKind, status: JobStatus);
}

use crate::routing::PlacementError;

/// Outcome of an ensure/delete fan-out (proxy-facing).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobOutcome {
    pub id: JobId,
    pub status: JobStatus,
}

/// Orchestrator failures mapped by the proxy / admin to 400/422/503/502.
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("job orchestrator is not configured")]
    NotConfigured,
    #[error("no placement-eligible target nodes for the requested models")]
    NoPlacementTargets,
    #[error("no target nodes")]
    NoTargetNodes,
    #[error("unknown node id: {0}")]
    UnknownNode(String),
    #[error("models must be non-empty")]
    EmptyModels,
    #[error("job not found")]
    NotFound,
    #[error("job is already terminal")]
    Conflict,
    #[error("{0}")]
    Other(String),
}

impl From<PlacementError> for OrchestratorError {
    fn from(err: PlacementError) -> Self {
        match err {
            PlacementError::UnknownNode(id) => Self::UnknownNode(id),
        }
    }
}

impl From<StoreError> for OrchestratorError {
    fn from(err: StoreError) -> Self {
        Self::Other(err.to_string())
    }
}

/// Boxed future so the trait stays object-safe.
pub type JobFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JobOutcome, OrchestratorError>> + Send + 'a>>;

/// Placement-aware pull/delete. Tests may inject [`StubOrchestrator`].
pub trait ModelOrchestrator: Send + Sync {
    fn ensure(&self, model: &str) -> JobFuture<'_>;
    fn delete(&self, model: &str) -> JobFuture<'_>;
}

/// Stub: pull/delete are not configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct StubOrchestrator;

impl ModelOrchestrator for StubOrchestrator {
    fn ensure(&self, _model: &str) -> JobFuture<'_> {
        Box::pin(std::future::ready(Err(OrchestratorError::NotConfigured)))
    }

    fn delete(&self, _model: &str) -> JobFuture<'_> {
        Box::pin(std::future::ready(Err(OrchestratorError::NotConfigured)))
    }
}

#[cfg(test)]
mod tests;
