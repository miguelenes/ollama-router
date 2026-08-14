//! SQLite WAL store for model-operation metadata (no bodies, no `detail`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use super::types::{Job, JobId, JobKind, JobStatus, JobTarget, TargetStatus};

const MIGRATION_001: &str = include_str!("migrations/001_model_operations.sql");

/// Store failures (path / sqlite). Never includes job `detail`.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("create job store directory: {0}")]
    CreateDir(String),
    #[error("open job store: {0}")]
    Open(String),
    #[error("job store sqlite: {0}")]
    Sqlite(String),
    #[error("job store blocking join: {0}")]
    Join(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err.to_string())
    }
}

/// WAL-backed operation records. `Mutex<Connection>` is only held inside
/// blocking sections (tests and `spawn_blocking`).
#[derive(Clone)]
pub struct JobStore {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl JobStore {
    /// Open (or create) the database, enable WAL, apply in-repo migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| StoreError::CreateDir(err.to_string()))?;
            }
        }
        let conn = Connection::open(&path).map_err(|err| StoreError::Open(err.to_string()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&conn)?;
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                conn: Mutex::new(conn),
            }),
        })
    }

    /// Filesystem path (tests).
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.inner
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn run_blocking<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(Self) -> Result<T, StoreError> + Send + 'static,
    {
        let store = self.clone();
        match tokio::task::spawn_blocking(move || f(store)).await {
            Ok(result) => result,
            Err(err) => Err(StoreError::Join(err.to_string())),
        }
    }

    /// Load every well-formed row. Malformed rows are skipped.
    ///
    /// Blocking rusqlite. HTTP/job tasks must use [`Self::load_async`].
    pub fn load(&self) -> Result<BTreeMap<JobId, Job>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, status, created_at, finished_at, models_json, nodes_json, targets_json
             FROM model_operations
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        let mut jobs = BTreeMap::new();
        for row in rows {
            let tuple = row?;
            if let Some(job) = row_to_job(tuple) {
                jobs.insert(job.id, job);
            }
        }
        Ok(jobs)
    }

    /// [`Self::load`] on the blocking pool.
    pub async fn load_async(&self) -> Result<BTreeMap<JobId, Job>, StoreError> {
        self.run_blocking(|store| store.load()).await
    }

    /// Upsert one snapshot with `detail` stripped.
    ///
    /// Blocking rusqlite. HTTP/job tasks must use [`Self::save_async`].
    pub fn save(&self, job: &Job) -> Result<(), StoreError> {
        let targets: BTreeMap<String, JobTarget> = job
            .targets
            .iter()
            .map(|(k, v)| (k.clone(), v.stripped()))
            .collect();
        let models_json = serde_json::to_string(&job.models).unwrap_or_else(|_| "[]".into());
        let nodes_json = serde_json::to_string(&job.nodes).unwrap_or_else(|_| "[]".into());
        let targets_json = serde_json::to_string(&targets).unwrap_or_else(|_| "{}".into());
        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO model_operations (
                id, kind, status, created_at, finished_at, models_json, nodes_json, targets_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                status = excluded.status,
                created_at = excluded.created_at,
                finished_at = excluded.finished_at,
                models_json = excluded.models_json,
                nodes_json = excluded.nodes_json,
                targets_json = excluded.targets_json",
            params![
                job.id.to_string(),
                job.kind.as_str(),
                job.status.as_str(),
                job.created_at,
                job.finished_at,
                models_json,
                nodes_json,
                targets_json,
            ],
        )?;
        Ok(())
    }

    /// [`Self::save`] on the blocking pool. Clones `job` only for the worker.
    pub async fn save_async(&self, job: &Job) -> Result<(), StoreError> {
        let job = job.clone();
        self.run_blocking(move |store| store.save(&job)).await
    }

    /// Remove a pruned terminal operation.
    ///
    /// Blocking rusqlite. HTTP/job tasks must use [`Self::delete_async`].
    pub fn delete(&self, id: &JobId) -> Result<(), StoreError> {
        let conn = self.lock_conn();
        conn.execute(
            "DELETE FROM model_operations WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// [`Self::delete`] on the blocking pool.
    pub async fn delete_async(&self, id: &JobId) -> Result<(), StoreError> {
        let id = *id;
        self.run_blocking(move |store| store.delete(&id)).await
    }

    /// Drop the operations table so a subsequent `save` fails (tests).
    #[cfg(test)]
    pub fn drop_table_for_tests(&self) -> Result<(), StoreError> {
        let conn = self.lock_conn();
        conn.execute_batch("DROP TABLE IF EXISTS model_operations")?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at REAL NOT NULL
        );",
    )?;
    let applied: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if applied.is_none() {
        conn.execute_batch(MIGRATION_001)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
            params![unix_now()],
        )?;
    }
    Ok(())
}

pub(crate) fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn row_to_job(
    row: (
        String,
        String,
        String,
        f64,
        Option<f64>,
        String,
        String,
        String,
    ),
) -> Option<Job> {
    let (
        id_raw,
        kind_raw,
        status_raw,
        created_at,
        finished_at,
        models_json,
        nodes_json,
        targets_json,
    ) = row;
    let id = JobId::parse(&id_raw).ok()?;
    let kind: JobKind = kind_raw.parse().ok()?;
    let status: JobStatus = status_raw.parse().ok()?;
    let models: Vec<String> = serde_json::from_str(&models_json).ok()?;
    let nodes: Vec<String> = serde_json::from_str(&nodes_json).ok()?;
    let stored: BTreeMap<String, serde_json::Value> = serde_json::from_str(&targets_json).ok()?;
    let mut targets = BTreeMap::new();
    for (key, value) in stored {
        let node = value.get("node")?.as_str()?.to_string();
        let model = value.get("model")?.as_str()?.to_string();
        let status: TargetStatus = value.get("status")?.as_str()?.parse().ok()?;
        targets.insert(
            key,
            JobTarget {
                node,
                model,
                status,
                detail: None,
            },
        );
    }
    Some(Job {
        id,
        kind,
        status,
        created_at,
        finished_at,
        models,
        nodes,
        targets,
    })
}
