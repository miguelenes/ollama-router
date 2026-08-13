//! Job orchestrator trait. SQLite persistence is a later slice.
//!
//! Store path (when implemented): `/var/lib/ollama-router/model-operations.sqlite3`.
//! No upstream bodies.

use std::future::Future;
use std::pin::Pin;

/// Outcome of an ensure/delete fan-out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobOutcome {
    pub id: String,
    pub status: JobStatus,
}

/// Terminal job status (wire-facing).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobStatus {
    Success,
    Failed,
}

/// Orchestrator failures mapped by the proxy to 400/503/502.
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("job orchestrator is not configured")]
    NotConfigured,
    #[error("no placement-eligible target nodes for the requested models")]
    NoPlacementTargets,
    #[error("no target nodes")]
    NoTargetNodes,
    #[error("{0}")]
    Other(String),
}

/// Boxed future so the trait stays object-safe.
pub type JobFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JobOutcome, OrchestratorError>> + Send + 'a>>;

/// Placement-aware pull/delete. SQLite-backed impl comes later; tests inject a fake.
pub trait ModelOrchestrator: Send + Sync {
    fn ensure(&self, model: &str) -> JobFuture<'_>;
    fn delete(&self, model: &str) -> JobFuture<'_>;
}

/// Default stub: pull/delete are not configured yet.
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
