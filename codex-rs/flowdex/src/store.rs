use crate::context::{
    ContextError, ContextFragment, ContextPackDeclaration, ContextPackStatus, ContextPublication,
    ContextPublisher, ContextStaleSource, ResolvedContextPack, read_source_range,
    validate_publication,
};
use crate::workflow::{
    TaskDefinition, WorkflowDefinition, WorkflowValidationError, validate_task_definition,
    write_scope_conflicts,
};
use crate::{RuleCandidate, RuleCandidateEvidence, RuleCandidateScanResult};
use sha2::Digest;
use sha2::Sha256;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use thiserror::Error;

const MAX_GIT_OUTPUT: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum FlowdexStoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("unable to access task path: {0}")]
    Io(#[from] std::io::Error),
    #[error("git command failed: {0}")]
    Git(String),
    #[error("task not found: {0}")]
    TaskNotFound(String),
    #[error("task operation is not available: {0}")]
    Operation(String),
    #[error("task cannot be integrated: {0}")]
    Integration(String),
    #[error("invalid workflow: {0}")]
    Workflow(#[from] WorkflowValidationError),
    #[error("invalid context publication: {0}")]
    Context(#[from] ContextError),
}

#[derive(Clone, Debug)]
pub struct RunInfo {
    pub run_id: String,
    pub parent_thread_id: String,
    pub workflow_path: String,
    pub parent_run_id: Option<String>,
    pub workflow_identity: Option<String>,
    pub repository_identity: String,
    pub integration_worktree: PathBuf,
}

#[derive(Clone, Debug)]
pub struct TaskDeclaration {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub read_scope: Vec<String>,
    pub write_scope: Vec<String>,
    pub verification: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct TaskRecord {
    pub id: String,
    pub run_id: String,
    pub name: String,
    pub instructions: String,
    pub read_scope: Vec<String>,
    pub write_scope: Vec<String>,
    pub verification: Vec<String>,
    pub base_commit: String,
    pub worktree_path: PathBuf,
    pub state: String,
    pub last_verified_commit: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TaskOperation {
    pub operation_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub model: String,
    pub start_commit: String,
}

#[derive(Clone, Debug)]
pub struct TaskCommit {
    pub source_commit: String,
    pub integrated_commit: Option<String>,
    pub agent_id: String,
    pub model: String,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct IntegrationResult {
    pub task_id: String,
    pub commits: Vec<TaskCommit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    Queued,
    Running,
    Verifying,
    Completed,
    Failed,
}
impl RunState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseState {
    Pending,
    Running,
    Verifying,
    Waiting,
    Sealed,
    Completed,
    Failed,
}
impl PhaseState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Waiting => "waiting",
            Self::Sealed => "sealed",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerTaskState {
    Queued,
    Ready,
    Running,
    Attributing,
    Verified,
    Integrated,
    Failed,
}
impl SchedulerTaskState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Attributing => "attributing",
            Self::Verified => "verified",
            Self::Integrated => "integrated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledTask {
    pub task_id: String,
    pub phase: String,
    pub declaration_order: i64,
    pub agent: String,
    pub dependencies: Vec<String>,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledTaskDetails {
    pub task_id: String,
    pub run_id: String,
    pub phase: String,
    pub name: String,
    pub agent: String,
    pub instructions: String,
    pub dependencies: Vec<String>,
    pub read_scope: Vec<String>,
    pub write_scope: Vec<String>,
    pub verification: Vec<String>,
    pub declaration_order: i64,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunMetadata {
    pub run_id: String,
    pub name: String,
    pub verification: Vec<String>,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseMetadata {
    pub run_id: String,
    pub name: String,
    pub index: i64,
    pub total: i64,
    pub instructions: String,
    pub open: bool,
    pub sealed: bool,
    pub verification: Vec<String>,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewOperation {
    pub operation_id: String,
    pub run_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub round: i64,
    pub reviewer_thread_id: String,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewFinding {
    pub finding_id: String,
    pub operation_id: String,
    pub finding_order: i64,
    pub file: String,
    pub line_start: i64,
    pub line_end: i64,
    pub reason: String,
    pub rule_key: Option<String>,
    pub ast_grep_suitable: bool,
    pub attributed_task_id: Option<String>,
    pub attributed_operation_id: Option<String>,
    pub attributed_agent_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewResolution {
    pub finding_id: String,
    pub repair_operation_id: String,
    pub source_commit: String,
    pub integrated_commit: Option<String>,
}

/// A bounded patch for one explicitly supplied commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedDiff {
    pub source_commit: String,
    pub integrated_commit: Option<String>,
    pub patch: String,
}

/// The durable task/operation attribution for a review finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAttribution {
    pub finding_id: String,
    pub task_id: String,
    pub operation_id: String,
    pub agent_id: String,
    pub source_commit: String,
    pub integrated_commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingBoundary {
    pub run_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub target: String,
    pub reason: String,
    pub transition: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSignal {
    pub id: i64,
    pub signal: String,
}

pub struct FlowdexStore {
    pool: SqlitePool,
    runtime: ManuallyDrop<tokio::runtime::Runtime>,
    worktree_root: PathBuf,
    repository_identity: String,
    integration_worktree: PathBuf,
}

impl FlowdexStore {
    /// Opens the repository-local Flowdex database and verifies its identity.
    pub fn open(
        codex_home: &Path,
        repository_identity: impl Into<String>,
        integration_worktree: &Path,
    ) -> Result<Self, FlowdexStoreError> {
        let repository_identity = repository_identity.into();
        let key = repository_key(&repository_identity);
        let flowdex_root = codex_home.join("flowdex");
        let worktree_root = flowdex_root.join("worktrees").join(&key);
        fs::create_dir_all(&worktree_root)?;
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|error| FlowdexStoreError::Integration(error.to_string()))?;
        let database_path = flowdex_root.join(format!("{key}.sqlite"));
        let options = SqliteConnectOptions::new()
            .filename(database_path)
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(30));
        let pool = runtime.block_on(
            SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options),
        )?;
        runtime.block_on(migrate(&pool))?;
        let existing: Option<String> = runtime.block_on(async {
            sqlx::query_scalar("SELECT identity FROM repository LIMIT 1")
                .fetch_optional(&pool)
                .await
        })?;
        if let Some(existing) = existing {
            if existing != repository_identity {
                return Err(FlowdexStoreError::Integration(
                    "Flowdex database repository identity mismatch".to_string(),
                ));
            }
        } else {
            runtime.block_on(
                sqlx::query("INSERT INTO repository(identity) VALUES (?)")
                    .bind(&repository_identity)
                    .execute(&pool),
            )?;
        }
        Ok(Self {
            pool,
            runtime: ManuallyDrop::new(runtime),
            worktree_root,
            repository_identity,
            integration_worktree: integration_worktree.to_path_buf(),
        })
    }

    pub fn ensure_run(&self, info: &RunInfo) -> Result<(), FlowdexStoreError> {
        if info.repository_identity != self.repository_identity {
            return Err(FlowdexStoreError::Integration(
                "run repository identity does not match task store".to_string(),
            ));
        }
        self.runtime.block_on(sqlx::query("INSERT INTO runs(run_id,parent_thread_id,workflow_path,parent_run_id,workflow_identity,repository_identity,integration_worktree,created_at) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(run_id) DO UPDATE SET parent_thread_id=excluded.parent_thread_id,workflow_path=excluded.workflow_path,parent_run_id=excluded.parent_run_id,workflow_identity=excluded.workflow_identity,integration_worktree=excluded.integration_worktree")
            .bind(&info.run_id).bind(&info.parent_thread_id).bind(&info.workflow_path).bind(&info.parent_run_id).bind(&info.workflow_identity).bind(&info.repository_identity).bind(info.integration_worktree.to_string_lossy().as_ref()).bind(now_unix()).execute(&self.pool))?;
        Ok(())
    }

    /// Loads the persisted identity and integration worktree for a run.
    pub fn run_info(&self, run_id: &str) -> Result<RunInfo, FlowdexStoreError> {
        let row = self
            .runtime
            .block_on(sqlx::query("SELECT run_id,parent_thread_id,workflow_path,parent_run_id,workflow_identity,repository_identity,integration_worktree FROM runs WHERE run_id=?")
                .bind(run_id)
                .fetch_optional(&self.pool))?
            .ok_or_else(|| FlowdexStoreError::Integration(format!("run not found: {run_id}")))?;
        Ok(RunInfo {
            run_id: row.get(0),
            parent_thread_id: row.get(1),
            workflow_path: row.get(2),
            parent_run_id: row.get(3),
            workflow_identity: row.get(4),
            repository_identity: row.get(5),
            integration_worktree: PathBuf::from(row.get::<String, _>(6)),
        })
    }

    /// Declares the context packs available to a run.
    pub fn declare_context_packs(
        &self,
        run_id: &str,
        declarations: &[(String, ContextPackDeclaration)],
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            for (pack, declaration) in declarations {
                if pack.trim().is_empty() {
                    return Err(FlowdexStoreError::Context(ContextError::EmptyField("pack")));
                }
                if declaration.agent.trim().is_empty() {
                    return Err(FlowdexStoreError::Context(ContextError::EmptyField("agent")));
                }
                if declaration.instructions.trim().is_empty() {
                    return Err(FlowdexStoreError::Context(ContextError::EmptyField("instructions")));
                }
                let agent_exists: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM workflow_agents WHERE run_id=? AND name=?",
                )
                .bind(run_id)
                .bind(&declaration.agent)
                .fetch_one(&mut *tx)
                .await?;
                if agent_exists == 0 {
                    return Err(FlowdexStoreError::Integration(format!(
                        "context pack agent not found: {}",
                        declaration.agent
                    )));
                }
                sqlx::query(
                    "INSERT INTO context_packs(run_id,name,agent,instructions) VALUES (?,?,?,?) ON CONFLICT(run_id,name) DO UPDATE SET agent=excluded.agent,instructions=excluded.instructions",
                )
                .bind(run_id)
                .bind(pack)
                .bind(&declaration.agent)
                .bind(&declaration.instructions)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            Ok::<(), FlowdexStoreError>(())
        })?;
        Ok(())
    }

    /// Publishes an immutable context fragment from an explicit execution worktree.
    pub fn publish_context_fragment(
        &self,
        run_id: &str,
        execution_worktree: &Path,
        trusted_repository_root: &Path,
        publisher: &ContextPublisher,
        publication: &ContextPublication,
    ) -> Result<ContextFragment, FlowdexStoreError> {
        validate_publication(publication)?;
        let trusted_root = trusted_repository_root.canonicalize()?;
        let execution_root = execution_worktree.canonicalize()?;
        if !trusted_root.is_dir() {
            return Err(FlowdexStoreError::Integration(
                "trusted repository root is not a directory".into(),
            ));
        }
        if !execution_root.is_dir() {
            return Err(FlowdexStoreError::Integration(
                "execution worktree is not a directory".into(),
            ));
        }
        // The trusted root is intentionally validated independently. The source
        // is read from the explicit execution worktree, which may be a sibling.
        let content = read_source_range(
            &execution_root,
            &publication.path,
            publication.line_start,
            publication.line_end,
        )?;
        let content_hash = hash_content(&content);
        Ok(self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            let pack_exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM context_packs WHERE run_id=? AND name=?",
            )
            .bind(run_id)
            .bind(&publication.pack)
            .fetch_one(&mut *tx)
            .await?;
            if pack_exists == 0 {
                return Err(FlowdexStoreError::Integration(format!(
                    "context pack not declared: {}",
                    publication.pack
                )));
            }
            let previous: Option<i64> = sqlx::query_scalar(
                "SELECT MAX(version) FROM context_fragments WHERE run_id=? AND pack=? AND fragment_key=?",
            )
            .bind(run_id)
            .bind(&publication.pack)
            .bind(&publication.key)
            .fetch_one(&mut *tx)
            .await?;
            let version = previous.unwrap_or(0) + 1;
            sqlx::query("INSERT INTO context_fragments(run_id,pack,fragment_key,version,publisher_thread_id,publisher_agent_id,path,line_start,line_end,summary,content,content_hash,superseded_version,created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind(run_id)
                .bind(&publication.pack)
                .bind(&publication.key)
                .bind(version)
                .bind(&publisher.thread_id)
                .bind(&publisher.agent_id)
                .bind(publication.path.to_string_lossy().as_ref())
                .bind(publication.line_start as i64)
                .bind(publication.line_end as i64)
                .bind(&publication.summary)
                .bind(&content)
                .bind(&content_hash)
                .bind(previous)
                .bind(now_unix())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(ContextFragment {
                pack: publication.pack.clone(),
                key: publication.key.clone(),
                version,
                path: publication.path.clone(),
                line_start: publication.line_start,
                line_end: publication.line_end,
                summary: publication.summary.clone(),
                content,
                content_hash,
            })
        })?)
    }

    /// Resolves active fragments against the supplied integration worktree.
    pub fn resolve_context_pack(
        &self,
        run_id: &str,
        pack: &str,
        integration_worktree: &Path,
    ) -> Result<ResolvedContextPack, FlowdexStoreError> {
        let rows = self.runtime.block_on(
            sqlx::query("SELECT f.fragment_key,f.version,f.path,f.line_start,f.line_end,f.summary,f.content,f.content_hash FROM context_fragments f JOIN (SELECT fragment_key,MAX(version) AS version FROM context_fragments WHERE run_id=? AND pack=? GROUP BY fragment_key) active ON active.fragment_key=f.fragment_key AND active.version=f.version WHERE f.run_id=? AND f.pack=? ORDER BY f.fragment_key")
                .bind(run_id).bind(pack).bind(run_id).bind(pack).fetch_all(&self.pool),
        )?;
        if rows.is_empty() {
            return Ok(ResolvedContextPack {
                pack: pack.to_string(),
                status: ContextPackStatus::Missing,
                fragments: Vec::new(),
                stale_sources: Vec::new(),
            });
        }
        let root = integration_worktree.canonicalize()?;
        let mut fragments = Vec::with_capacity(rows.len());
        let mut stale_sources = Vec::new();
        for row in rows {
            let path: PathBuf = PathBuf::from(row.get::<String, _>(2));
            let line_start = row.get::<i64, _>(3) as u32;
            let line_end = row.get::<i64, _>(4) as u32;
            let content = row.get::<String, _>(6);
            let content_hash = row.get::<String, _>(7);
            let fresh = read_source_range(&root, &path, line_start, line_end)
                .map(|current| hash_content(&current) == content_hash)
                .unwrap_or(false);
            if !fresh {
                stale_sources.push(ContextStaleSource {
                    key: row.get(0),
                    path: path.clone(),
                    line_start,
                    line_end,
                });
            }
            fragments.push(ContextFragment {
                pack: pack.to_string(),
                key: row.get(0),
                version: row.get(1),
                path,
                line_start,
                line_end,
                summary: row.get(5),
                content,
                content_hash,
            });
        }
        Ok(ResolvedContextPack {
            pack: pack.to_string(),
            status: if stale_sources.is_empty() {
                ContextPackStatus::Fresh
            } else {
                ContextPackStatus::Stale
            },
            fragments,
            stale_sources,
        })
    }

    /// Persists the ordered workflow graph in one transaction before any work starts.
    pub fn initialize_workflow(
        &self,
        info: &RunInfo,
        definition: &WorkflowDefinition,
    ) -> Result<(), FlowdexStoreError> {
        definition.validate()?;
        self.ensure_run(info)?;
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            sqlx::query("UPDATE runs SET name=?, verification=?, state=?, boundary=? WHERE run_id=?")
                .bind(&definition.name).bind(encode(&definition.verification)).bind(RunState::Queued.as_str()).bind(boundary_name(definition.boundary)).bind(&info.run_id)
                .execute(&mut *tx).await?;
            for (agent_name, agent) in &definition.agents {
                sqlx::query("INSERT INTO workflow_agents(run_id,name,profile,model,reasoning_effort,tool_profile) VALUES (?,?,?,?,?,?)")
                    .bind(&info.run_id).bind(agent_name).bind(agent.profile.as_deref()).bind(agent.model.as_deref()).bind(agent.reasoning_effort.as_deref()).bind(agent.tool_profile.as_deref()).execute(&mut *tx).await?;
            }
            for (index, phase) in definition.phases.iter().enumerate() {
                sqlx::query("INSERT INTO workflow_phases(run_id,name,declaration_order,instructions,open,sealed,verification,state,boundary,review_agent,review_instructions,review_max_rounds) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)")
                    .bind(&info.run_id).bind(&phase.name).bind(index as i64).bind(&phase.instructions)
                    .bind(phase.open as i64).bind((!phase.open) as i64).bind(encode(&phase.verification)).bind(if phase.open { PhaseState::Pending.as_str() } else { PhaseState::Sealed.as_str() })
                    .bind(boundary_name(phase.boundary)).bind(phase.review.as_ref().map(|review| review.agent.as_str())).bind(phase.review.as_ref().map(|review| review.instructions.as_str())).bind(phase.review.as_ref().map(|review| review.max_rounds as i64))
                    .execute(&mut *tx).await?;
                for (task_index, task) in phase.tasks.iter().enumerate() {
                    let task_id = uuid::Uuid::new_v4().to_string();
                    insert_schedule_row(&mut tx, &task_id, &info.run_id, &phase.name, &phase.instructions, task_index as i64, task, SchedulerTaskState::Queued).await?;
                }
            }
            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        })?;
        Ok(())
    }

    /// Adds a task to an open phase. Validation and insertion are atomic.
    pub fn queue_task(
        &self,
        run_id: &str,
        phase_name: &str,
        task_id: &str,
        task: &TaskDefinition,
    ) -> Result<(), FlowdexStoreError> {
        validate_task_definition(task)?;
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            let phase = sqlx::query("SELECT open,sealed,instructions FROM workflow_phases WHERE run_id=? AND name=?")
                .bind(run_id).bind(phase_name).fetch_optional(&mut *tx).await?
                .ok_or_else(|| FlowdexStoreError::Integration(format!("phase not found: {phase_name}")))?;
            if phase.get::<i64, _>(0) == 0 || phase.get::<i64, _>(1) != 0 { return Err(FlowdexStoreError::Workflow(WorkflowValidationError::PhaseNotOpen(phase_name.to_string()))); }
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_tasks WHERE run_id=? AND phase=? AND name=?").bind(run_id).bind(phase_name).bind(&task.name).fetch_one(&mut *tx).await?;
            if count != 0 { return Err(FlowdexStoreError::Integration(format!("duplicate task: {}", task.name))); }
            let agent_exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_agents WHERE run_id=? AND name=?").bind(run_id).bind(&task.agent).fetch_one(&mut *tx).await?;
            if agent_exists == 0 { return Err(FlowdexStoreError::Workflow(WorkflowValidationError::UnknownAgent { task: task.name.clone(), agent: task.agent.clone() })); }
            for dependency in &task.dependencies {
                let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_tasks WHERE run_id=? AND phase=? AND name=?").bind(run_id).bind(phase_name).bind(dependency).fetch_one(&mut *tx).await?;
                if exists == 0 { return Err(FlowdexStoreError::Workflow(WorkflowValidationError::UnknownDependency { phase: phase_name.to_string(), task: task.name.clone(), dependency: dependency.clone() })); }
            }
            let order: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(declaration_order),-1)+1 FROM workflow_tasks WHERE run_id=? AND phase=?").bind(run_id).bind(phase_name).fetch_one(&mut *tx).await?;
            insert_schedule_row(&mut tx, task_id, run_id, phase_name, phase.get(2), order, task, SchedulerTaskState::Queued).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    pub fn seal_phase(&self, run_id: &str, phase_name: &str) -> Result<(), FlowdexStoreError> {
        let changed = self.runtime.block_on(sqlx::query("UPDATE workflow_phases SET sealed=1,state='sealed' WHERE run_id=? AND name=? AND open=1 AND sealed=0").bind(run_id).bind(phase_name).execute(&self.pool))?;
        if changed.rows_affected() == 0 {
            return Err(FlowdexStoreError::Integration(format!(
                "phase is not open: {phase_name}"
            )));
        }
        Ok(())
    }

    pub fn set_run_state(&self, run_id: &str, state: RunState) -> Result<(), FlowdexStoreError> {
        let result = self.runtime.block_on(
            sqlx::query("UPDATE runs SET state=? WHERE run_id=?")
                .bind(state.as_str())
                .bind(run_id)
                .execute(&self.pool),
        )?;
        if result.rows_affected() == 0 {
            return Err(FlowdexStoreError::Integration(format!(
                "run not found: {run_id}"
            )));
        }
        Ok(())
    }

    pub fn set_phase_state(
        &self,
        run_id: &str,
        phase_name: &str,
        state: PhaseState,
    ) -> Result<(), FlowdexStoreError> {
        let result = self.runtime.block_on(
            sqlx::query("UPDATE workflow_phases SET state=? WHERE run_id=? AND name=?")
                .bind(state.as_str())
                .bind(run_id)
                .bind(phase_name)
                .execute(&self.pool),
        )?;
        if result.rows_affected() == 0 {
            return Err(FlowdexStoreError::Integration(format!(
                "phase not found: {phase_name}"
            )));
        }
        Ok(())
    }

    pub fn set_scheduler_task_state(
        &self,
        task_id: &str,
        state: SchedulerTaskState,
    ) -> Result<(), FlowdexStoreError> {
        let result = self.runtime.block_on(
            sqlx::query("UPDATE workflow_tasks SET state=? WHERE task_id=?")
                .bind(state.as_str())
                .bind(task_id)
                .execute(&self.pool),
        )?;
        if result.rows_affected() == 0 {
            return Err(FlowdexStoreError::TaskNotFound(task_id.to_string()));
        }
        Ok(())
    }

    pub fn scheduler_task(&self, task_id: &str) -> Result<ScheduledTaskDetails, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("SELECT t.task_id,t.run_id,t.phase,t.name,t.agent,t.instructions,t.dependencies,t.read_scope,t.write_scope,t.verification,t.declaration_order,t.state FROM workflow_tasks t WHERE t.task_id=?").bind(task_id).fetch_optional(&self.pool))?.ok_or_else(|| FlowdexStoreError::TaskNotFound(task_id.to_string()))?;
        Ok(ScheduledTaskDetails {
            task_id: row.get(0),
            run_id: row.get(1),
            phase: row.get(2),
            name: row.get(3),
            agent: row.get(4),
            instructions: row.get(5),
            dependencies: decode(row.get(6)),
            read_scope: decode(row.get(7)),
            write_scope: decode(row.get(8)),
            verification: decode(row.get(9)),
            declaration_order: row.get(10),
            state: row.get(11),
        })
    }

    pub fn workflow_agent(
        &self,
        run_id: &str,
        name: &str,
    ) -> Result<crate::workflow::AgentDefinition, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("SELECT profile,model,reasoning_effort,tool_profile FROM workflow_agents WHERE run_id=? AND name=?").bind(run_id).bind(name).fetch_optional(&self.pool))?.ok_or_else(|| FlowdexStoreError::Integration(format!("agent not found: {name}")))?;
        Ok(crate::workflow::AgentDefinition {
            profile: row.get(0),
            model: row.get(1),
            reasoning_effort: row.get(2),
            tool_profile: row.get(3),
        })
    }

    pub fn enqueue_signal(&self, run_id: &str, signal: &str) -> Result<(), FlowdexStoreError> {
        if signal.trim().is_empty() {
            return Err(FlowdexStoreError::Operation(
                "signal name must be non-empty".to_string(),
            ));
        }
        self.runtime.block_on(
            sqlx::query("INSERT INTO pending_signals(run_id,signal) VALUES (?,?)")
                .bind(run_id)
                .bind(signal)
                .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn oldest_pending_signal(
        &self,
        run_id: &str,
    ) -> Result<Option<PendingSignal>, FlowdexStoreError> {
        let row = self.runtime.block_on(
            sqlx::query(
                "SELECT signal_id,signal FROM pending_signals WHERE run_id=? ORDER BY signal_id LIMIT 1",
            )
            .bind(run_id)
            .fetch_optional(&self.pool),
        )?;
        Ok(row.map(|row| PendingSignal {
            id: row.get(0),
            signal: row.get(1),
        }))
    }

    pub fn consume_signal(&self, run_id: &str, signal_id: i64) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(
            sqlx::query("DELETE FROM pending_signals WHERE run_id=? AND signal_id=?")
                .bind(run_id)
                .bind(signal_id)
                .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn restore_signal(
        &self,
        run_id: &str,
        signal_id: i64,
        signal: &str,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(
            sqlx::query("INSERT INTO pending_signals(signal_id,run_id,signal) VALUES(?,?,?)")
                .bind(signal_id)
                .bind(run_id)
                .bind(signal)
                .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn run_metadata(&self, run_id: &str) -> Result<RunMetadata, FlowdexStoreError> {
        let row = self
            .runtime
            .block_on(
                sqlx::query("SELECT run_id,name,verification,state FROM runs WHERE run_id=?")
                    .bind(run_id)
                    .fetch_optional(&self.pool),
            )?
            .ok_or_else(|| FlowdexStoreError::Integration(format!("run not found: {run_id}")))?;
        Ok(RunMetadata {
            run_id: row.get(0),
            name: row.get(1),
            verification: decode(row.get(2)),
            state: row.get(3),
        })
    }

    pub fn phase_metadata(
        &self,
        run_id: &str,
        phase_name: &str,
    ) -> Result<PhaseMetadata, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("SELECT run_id,name,declaration_order,instructions,open,sealed,verification,state,(SELECT COUNT(*) FROM workflow_phases p2 WHERE p2.run_id=workflow_phases.run_id) FROM workflow_phases WHERE run_id=? AND name=?").bind(run_id).bind(phase_name).fetch_optional(&self.pool))?.ok_or_else(|| FlowdexStoreError::Integration(format!("phase not found: {phase_name}")))?;
        Ok(PhaseMetadata {
            run_id: row.get(0),
            name: row.get(1),
            index: row.get(2),
            total: row.get(8),
            instructions: row.get(3),
            open: row.get::<i64, _>(4) != 0,
            sealed: row.get::<i64, _>(5) != 0,
            verification: decode(row.get(6)),
            state: row.get(7),
        })
    }

    pub fn mark_task_running(&self, task_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_scheduler_task_state(task_id, SchedulerTaskState::Running)
    }
    pub fn mark_task_ready(&self, task_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_scheduler_task_state(task_id, SchedulerTaskState::Ready)
    }
    pub fn mark_task_attributing(&self, task_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_scheduler_task_state(task_id, SchedulerTaskState::Attributing)
    }
    pub fn mark_task_integrated(&self, task_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_scheduler_task_state(task_id, SchedulerTaskState::Integrated)
    }
    pub fn mark_task_failed(&self, task_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_scheduler_task_state(task_id, SchedulerTaskState::Failed)
    }
    pub fn mark_phase_running(&self, run_id: &str, phase: &str) -> Result<(), FlowdexStoreError> {
        self.set_phase_state(run_id, phase, PhaseState::Running)
    }
    pub fn mark_phase_verifying(&self, run_id: &str, phase: &str) -> Result<(), FlowdexStoreError> {
        self.set_phase_state(run_id, phase, PhaseState::Verifying)
    }
    pub fn mark_phase_completed(&self, run_id: &str, phase: &str) -> Result<(), FlowdexStoreError> {
        self.set_phase_state(run_id, phase, PhaseState::Completed)
    }
    pub fn mark_phase_failed(&self, run_id: &str, phase: &str) -> Result<(), FlowdexStoreError> {
        self.set_phase_state(run_id, phase, PhaseState::Failed)
    }
    pub fn mark_run_running(&self, run_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_run_state(run_id, RunState::Running)
    }
    pub fn mark_run_verifying(&self, run_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_run_state(run_id, RunState::Verifying)
    }
    pub fn mark_run_completed(&self, run_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_run_state(run_id, RunState::Completed)
    }
    pub fn mark_run_failed(&self, run_id: &str) -> Result<(), FlowdexStoreError> {
        self.set_run_state(run_id, RunState::Failed)
    }

    pub fn record_review_operation(
        &self,
        operation: &ReviewOperation,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(sqlx::query("INSERT INTO review_operations(operation_id,run_id,scope_kind,scope_id,round,reviewer_thread_id,state) VALUES (?,?,?,?,?,?,?) ON CONFLICT(operation_id) DO UPDATE SET state=CASE WHEN review_operations.state='accepted' THEN review_operations.state ELSE excluded.state END,reviewer_thread_id=excluded.reviewer_thread_id")
            .bind(&operation.operation_id).bind(&operation.run_id).bind(&operation.scope_kind).bind(&operation.scope_id)
            .bind(operation.round).bind(&operation.reviewer_thread_id).bind(&operation.state).execute(&self.pool))?;
        Ok(())
    }

    pub fn review_operation(
        &self,
        operation_id: &str,
    ) -> Result<ReviewOperation, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("SELECT operation_id,run_id,scope_kind,scope_id,round,reviewer_thread_id,state FROM review_operations WHERE operation_id=?").bind(operation_id).fetch_optional(&self.pool))?
            .ok_or_else(|| FlowdexStoreError::Operation(format!("review operation not found: {operation_id}")))?;
        Ok(ReviewOperation {
            operation_id: row.get(0),
            run_id: row.get(1),
            scope_kind: row.get(2),
            scope_id: row.get(3),
            round: row.get(4),
            reviewer_thread_id: row.get(5),
            state: row.get(6),
        })
    }

    pub fn record_review_findings(
        &self,
        findings: &[ReviewFinding],
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            for finding in findings {
                if finding.line_start <= 0 || finding.line_end < finding.line_start {
                    return Err(FlowdexStoreError::Integration("invalid review finding lines".into()));
                }
                sqlx::query("INSERT INTO review_findings(finding_id,operation_id,finding_order,file,line_start,line_end,reason,rule_key,ast_grep_suitable,attributed_task_id,attributed_operation_id,attributed_agent_id) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(finding_id) DO UPDATE SET attributed_task_id=excluded.attributed_task_id,attributed_operation_id=excluded.attributed_operation_id,attributed_agent_id=excluded.attributed_agent_id")
                    .bind(&finding.finding_id).bind(&finding.operation_id).bind(finding.finding_order).bind(&finding.file)
                    .bind(finding.line_start).bind(finding.line_end).bind(&finding.reason).bind(&finding.rule_key)
                    .bind(finding.ast_grep_suitable as i64).bind(&finding.attributed_task_id).bind(&finding.attributed_operation_id).bind(&finding.attributed_agent_id)
                    .execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok::<(), FlowdexStoreError>(())
        })
    }

    pub fn review_findings(
        &self,
        operation_id: &str,
    ) -> Result<Vec<ReviewFinding>, FlowdexStoreError> {
        let rows = self.runtime.block_on(sqlx::query("SELECT finding_id,operation_id,finding_order,file,line_start,line_end,reason,rule_key,ast_grep_suitable,attributed_task_id,attributed_operation_id,attributed_agent_id FROM review_findings WHERE operation_id=? ORDER BY finding_order").bind(operation_id).fetch_all(&self.pool))?;
        Ok(rows
            .into_iter()
            .map(|row| ReviewFinding {
                finding_id: row.get(0),
                operation_id: row.get(1),
                finding_order: row.get(2),
                file: row.get(3),
                line_start: row.get(4),
                line_end: row.get(5),
                reason: row.get(6),
                rule_key: row.get(7),
                ast_grep_suitable: row.get::<i64, _>(8) != 0,
                attributed_task_id: row.get(9),
                attributed_operation_id: row.get(10),
                attributed_agent_id: row.get(11),
            })
            .collect())
    }

    pub fn record_review_resolution(
        &self,
        resolution: &ReviewResolution,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            let finding_exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM review_findings WHERE finding_id=?",
            )
            .bind(&resolution.finding_id)
            .fetch_one(&mut *tx)
            .await?;
            if finding_exists == 0 {
                return Err(FlowdexStoreError::Integration(format!(
                    "review finding not found: {}",
                    resolution.finding_id
                )));
            }
            let repair_exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM task_commits WHERE operation_id=? AND source_commit=?",
            )
            .bind(&resolution.repair_operation_id)
            .bind(&resolution.source_commit)
            .fetch_one(&mut *tx)
            .await?;
            if repair_exists == 0 {
                return Err(FlowdexStoreError::Integration(format!(
                    "repair commit is not attributed to operation: {}:{}",
                    resolution.repair_operation_id, resolution.source_commit
                )));
            }
            sqlx::query("INSERT INTO review_resolutions(finding_id,repair_operation_id,source_commit,integrated_commit) VALUES (?,?,?,?) ON CONFLICT(finding_id,repair_operation_id,source_commit) DO UPDATE SET integrated_commit=excluded.integrated_commit")
                .bind(&resolution.finding_id).bind(&resolution.repair_operation_id).bind(&resolution.source_commit).bind(&resolution.integrated_commit).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok::<(), FlowdexStoreError>(())
        })?;
        Ok(())
    }

    pub fn review_resolutions(
        &self,
        finding_id: &str,
    ) -> Result<Vec<ReviewResolution>, FlowdexStoreError> {
        let rows = self.runtime.block_on(
            sqlx::query("SELECT finding_id,repair_operation_id,source_commit,integrated_commit FROM review_resolutions WHERE finding_id=? ORDER BY repair_operation_id,source_commit")
                .bind(finding_id)
                .fetch_all(&self.pool),
        )?;
        Ok(rows
            .into_iter()
            .map(|row| ReviewResolution {
                finding_id: row.get(0),
                repair_operation_id: row.get(1),
                source_commit: row.get(2),
                integrated_commit: row.get(3),
            })
            .collect())
    }

    /// Derives bounded rule candidates from durable, resolved review history.
    pub fn rule_candidates(
        &self,
        threshold: u64,
        approved_rule_ids: &BTreeSet<String>,
    ) -> Result<RuleCandidateScanResult, FlowdexStoreError> {
        if threshold == 0 {
            return Err(FlowdexStoreError::Integration(
                "candidate threshold must be positive".into(),
            ));
        }
        let rows = self.runtime.block_on(sqlx::query(
            "WITH ranked AS (
                SELECT f.finding_id,f.file,f.line_start,f.line_end,f.reason,f.rule_key,
                       r.source_commit,
                       COALESCE(r.integrated_commit,c.integrated_commit) AS integrated_commit,
                       ROW_NUMBER() OVER (
                           PARTITION BY f.finding_id
                           ORDER BY COALESCE(c.sequence, -1) DESC,
                                    r.source_commit DESC,
                                    r.repair_operation_id DESC
                       ) AS representative
                FROM review_findings f
                JOIN review_resolutions r ON r.finding_id=f.finding_id
                LEFT JOIN task_commits c
                  ON c.operation_id=r.repair_operation_id
                 AND c.source_commit=r.source_commit
                WHERE f.ast_grep_suitable=1
                  AND TRIM(COALESCE(f.rule_key,'')) <> ''
                  AND COALESCE(r.integrated_commit,c.integrated_commit) IS NOT NULL
            )
            SELECT finding_id,file,line_start,line_end,reason,rule_key,source_commit,integrated_commit
            FROM ranked
            WHERE representative=1
            ORDER BY rule_key,finding_id,file,line_start,line_end,source_commit,integrated_commit",
        ).fetch_all(&self.pool))?;

        let mut grouped: BTreeMap<String, Vec<RuleCandidateEvidence>> = BTreeMap::new();
        for row in rows {
            let rule_key: String = row.get(5);
            if approved_rule_ids.contains(&rule_key) {
                continue;
            }
            grouped
                .entry(rule_key)
                .or_default()
                .push(RuleCandidateEvidence {
                    file: row.get(1),
                    line_start: row.get(2),
                    line_end: row.get(3),
                    reason: row.get(4),
                    source_commit: row.get(6),
                    integrated_commit: row.get(7),
                });
        }

        let threshold = usize::try_from(threshold).unwrap_or(usize::MAX);
        let candidates = grouped
            .into_iter()
            .filter(|(_, examples)| examples.len() >= threshold)
            .take(50)
            .map(|(rule_key, mut examples)| RuleCandidate {
                rule_key,
                resolved_occurrences: examples.len() as u64,
                examples: {
                    examples.truncate(3);
                    examples
                },
            })
            .collect();
        Ok(RuleCandidateScanResult { candidates })
    }

    /// Returns bounded patches for the supplied task commits. No HEAD or range is inferred.
    pub fn committed_diffs(
        &self,
        worktree: &Path,
        commits: &[TaskCommit],
        max_bytes: usize,
    ) -> Result<Vec<CommittedDiff>, FlowdexStoreError> {
        let mut remaining = max_bytes;
        let mut diffs = Vec::with_capacity(commits.len());
        for commit in commits {
            let patch = commit_diff(worktree, &commit.source_commit, remaining)?;
            remaining = remaining.saturating_sub(patch.len());
            diffs.push(CommittedDiff {
                source_commit: commit.source_commit.clone(),
                integrated_commit: commit.integrated_commit.clone(),
                patch,
            });
        }
        Ok(diffs)
    }

    /// Returns bounded patches for explicitly supplied integrated phase commits.
    pub fn integrated_phase_diffs(
        &self,
        worktree: &Path,
        integrated_commits: &[String],
        max_bytes: usize,
    ) -> Result<Vec<CommittedDiff>, FlowdexStoreError> {
        let commits = integrated_commits
            .iter()
            .map(|commit| TaskCommit {
                source_commit: commit.clone(),
                integrated_commit: Some(commit.clone()),
                agent_id: String::new(),
                model: String::new(),
                summary: String::new(),
            })
            .collect::<Vec<_>>();
        self.committed_diffs(worktree, &commits, max_bytes)
    }

    /// Attributes a finding to the Flowdex commit that owns its reported lines.
    /// If no blamed line belongs to this run, the most recent run commit touching
    /// the file is used as the required file-level fallback.
    pub fn attribute_review_finding(
        &self,
        finding_id: &str,
        integration_worktree: &Path,
        integration_head: &str,
    ) -> Result<Option<ReviewAttribution>, FlowdexStoreError> {
        let row = self.runtime.block_on(
            sqlx::query("SELECT f.file,f.line_start,f.line_end,o.run_id FROM review_findings f JOIN review_operations o ON o.operation_id=f.operation_id WHERE f.finding_id=?")
                .bind(finding_id)
                .fetch_optional(&self.pool),
        )?.ok_or_else(|| FlowdexStoreError::Integration(format!("review finding not found: {finding_id}")))?;
        let file: String = row.get(0);
        let line_start: i64 = row.get(1);
        let line_end: i64 = row.get(2);
        let run_id: String = row.get(3);
        let mut commits = self.runtime.block_on(sqlx::query("SELECT c.source_commit,c.integrated_commit,c.task_id,c.operation_id,c.agent_id,c.sequence FROM task_commits c JOIN tasks t ON t.task_id=c.task_id WHERE t.run_id=? ORDER BY c.sequence DESC")
            .bind(&run_id).fetch_all(&self.pool))?;
        if commits.is_empty() {
            return Ok(None);
        }
        let history = git_stdout(
            integration_worktree,
            ["rev-list", "--first-parent", integration_head],
        )?;
        let order = history
            .lines()
            .enumerate()
            .map(|(index, commit)| (commit, index))
            .collect::<std::collections::HashMap<_, _>>();
        commits.sort_by_key(|row| {
            let commit: String = row
                .get::<Option<String>, _>(1)
                .unwrap_or_else(|| row.get(0));
            order.get(commit.as_str()).copied().unwrap_or(usize::MAX)
        });
        let mut selected = None;
        if let Ok(blame) = git_stdout(
            integration_worktree,
            [
                "blame",
                "--line-porcelain",
                &format!("-L{line_start},{line_end}"),
                "--format=%H",
                integration_head,
                "--",
                &file,
            ],
        ) {
            let blamed = blame
                .lines()
                .filter_map(|line| line.split_whitespace().next())
                .filter(|sha| sha.len() >= 7 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .collect::<std::collections::HashSet<_>>();
            for commit in &commits {
                let integrated: String = commit
                    .get::<Option<String>, _>(1)
                    .unwrap_or_else(|| commit.get(0));
                if blamed.contains(integrated.as_str()) {
                    selected = Some(commit);
                    break;
                }
            }
        }
        if selected.is_none() {
            let normalized_file = file.replace('\\', "/");
            for commit in &commits {
                let integrated: String = commit
                    .get::<Option<String>, _>(1)
                    .unwrap_or_else(|| commit.get(0));
                let files = git_stdout(
                    integration_worktree,
                    [
                        "diff-tree",
                        "--no-commit-id",
                        "--name-only",
                        "-r",
                        &integrated,
                    ],
                )?;
                if files.lines().any(|path| path == normalized_file) {
                    selected = Some(commit);
                    break;
                }
            }
        }
        let Some(commit) = selected else {
            return Ok(None);
        };
        let source_commit: String = commit.get(0);
        let attribution = ReviewAttribution {
            finding_id: finding_id.to_string(),
            source_commit: source_commit.clone(),
            integrated_commit: commit.get::<Option<String>, _>(1).unwrap_or(source_commit),
            task_id: commit.get(2),
            operation_id: commit.get(3),
            agent_id: commit.get(4),
        };
        self.runtime.block_on(
            sqlx::query("UPDATE review_findings SET attributed_task_id=?,attributed_operation_id=?,attributed_agent_id=? WHERE finding_id=?")
                .bind(&attribution.task_id)
                .bind(&attribution.operation_id)
                .bind(&attribution.agent_id)
                .bind(finding_id)
                .execute(&self.pool),
        )?;
        Ok(Some(attribution))
    }

    pub fn set_pending_boundary(
        &self,
        boundary: &PendingBoundary,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(sqlx::query("INSERT INTO pending_boundaries(run_id,scope_kind,scope_id,target,reason,transition) VALUES (?,?,?,?,?,?) ON CONFLICT(run_id,scope_kind,scope_id) DO UPDATE SET target=excluded.target,reason=excluded.reason,transition=excluded.transition")
            .bind(&boundary.run_id).bind(&boundary.scope_kind).bind(&boundary.scope_id).bind(&boundary.target).bind(&boundary.reason).bind(&boundary.transition).execute(&self.pool))?;
        Ok(())
    }

    pub fn pending_boundary(
        &self,
        run_id: &str,
    ) -> Result<Option<PendingBoundary>, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("SELECT run_id,scope_kind,scope_id,target,reason,transition FROM pending_boundaries WHERE run_id=? ORDER BY scope_kind,scope_id LIMIT 1").bind(run_id).fetch_optional(&self.pool))?;
        Ok(row.map(|row| PendingBoundary {
            run_id: row.get(0),
            scope_kind: row.get(1),
            scope_id: row.get(2),
            target: row.get(3),
            reason: row.get(4),
            transition: row.get(5),
        }))
    }

    pub fn clear_pending_boundary(
        &self,
        run_id: &str,
        scope_kind: &str,
        scope_id: &str,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(
            sqlx::query(
                "DELETE FROM pending_boundaries WHERE run_id=? AND scope_kind=? AND scope_id=?",
            )
            .bind(run_id)
            .bind(scope_kind)
            .bind(scope_id)
            .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn increment_verification_repairs(
        &self,
        scope_kind: &str,
        run_id: &str,
        scope_id: &str,
    ) -> Result<i64, FlowdexStoreError> {
        self.increment_scope_counter(scope_kind, run_id, scope_id, "verification_repair_count")
    }

    pub fn increment_review_rounds(
        &self,
        scope_kind: &str,
        run_id: &str,
        scope_id: &str,
    ) -> Result<i64, FlowdexStoreError> {
        self.increment_scope_counter(scope_kind, run_id, scope_id, "review_round_count")
    }

    pub fn increment_phase_verification_repairs(
        &self,
        run_id: &str,
        phase_name: &str,
    ) -> Result<i64, FlowdexStoreError> {
        self.increment_phase_counter(run_id, phase_name, "verification_repair_count")
    }

    pub fn increment_phase_review_rounds(
        &self,
        run_id: &str,
        phase_name: &str,
    ) -> Result<i64, FlowdexStoreError> {
        self.increment_phase_counter(run_id, phase_name, "review_round_count")
    }

    fn increment_phase_counter(
        &self,
        run_id: &str,
        phase_name: &str,
        column: &str,
    ) -> Result<i64, FlowdexStoreError> {
        let row = match column {
            "verification_repair_count" => self.runtime.block_on(sqlx::query("UPDATE workflow_phases SET verification_repair_count=verification_repair_count+1 WHERE run_id=? AND name=? RETURNING verification_repair_count").bind(run_id).bind(phase_name).fetch_optional(&self.pool))?,
            "review_round_count" => self.runtime.block_on(sqlx::query("UPDATE workflow_phases SET review_round_count=review_round_count+1 WHERE run_id=? AND name=? RETURNING review_round_count").bind(run_id).bind(phase_name).fetch_optional(&self.pool))?,
            _ => return Err(FlowdexStoreError::Integration(format!("unknown phase counter: {column}"))),
        };
        Ok(row
            .ok_or_else(|| {
                FlowdexStoreError::Integration(format!("phase not found: {run_id}:{phase_name}"))
            })?
            .get(0))
    }

    fn increment_scope_counter(
        &self,
        scope_kind: &str,
        run_id: &str,
        scope_id: &str,
        column: &str,
    ) -> Result<i64, FlowdexStoreError> {
        let row = match (scope_kind, column) {
            ("run", "verification_repair_count") => self.runtime.block_on(sqlx::query("UPDATE runs SET verification_repair_count=verification_repair_count+1 WHERE run_id=? RETURNING verification_repair_count").bind(run_id).fetch_optional(&self.pool))?,
            ("run", "review_round_count") => self.runtime.block_on(sqlx::query("UPDATE runs SET review_round_count=review_round_count+1 WHERE run_id=? RETURNING review_round_count").bind(run_id).fetch_optional(&self.pool))?,
            ("phase", "verification_repair_count") => self.runtime.block_on(sqlx::query("UPDATE workflow_phases SET verification_repair_count=verification_repair_count+1 WHERE run_id=? AND name=? RETURNING verification_repair_count").bind(run_id).bind(scope_id).fetch_optional(&self.pool))?,
            ("phase", "review_round_count") => self.runtime.block_on(sqlx::query("UPDATE workflow_phases SET review_round_count=review_round_count+1 WHERE run_id=? AND name=? RETURNING review_round_count").bind(run_id).bind(scope_id).fetch_optional(&self.pool))?,
            ("task", "verification_repair_count") => self.runtime.block_on(sqlx::query("UPDATE workflow_tasks SET verification_repair_count=verification_repair_count+1 WHERE run_id=? AND task_id=? RETURNING verification_repair_count").bind(run_id).bind(scope_id).fetch_optional(&self.pool))?,
            ("task", "review_round_count") => self.runtime.block_on(sqlx::query("UPDATE workflow_tasks SET review_round_count=review_round_count+1 WHERE run_id=? AND task_id=? RETURNING review_round_count").bind(run_id).bind(scope_id).fetch_optional(&self.pool))?,
            _ => return Err(FlowdexStoreError::Integration(format!("unknown scope: {scope_kind}:{scope_id}"))),
        };
        let row = row.ok_or_else(|| {
            FlowdexStoreError::Integration(format!("scope not found: {scope_kind}:{scope_id}"))
        })?;
        Ok(row.get(0))
    }

    /// Marks queued tasks ready only after all dependency tasks integrated.
    pub fn ready_tasks(
        &self,
        run_id: &str,
        phase_name: &str,
    ) -> Result<Vec<ScheduledTask>, FlowdexStoreError> {
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            let rows = sqlx::query("SELECT task_id,phase,declaration_order,agent,dependencies,state,name,write_scope FROM workflow_tasks WHERE run_id=? AND phase=? ORDER BY declaration_order")
                .bind(run_id).bind(phase_name).fetch_all(&mut *tx).await?;
            let states: std::collections::HashMap<String, String> = rows.iter().map(|row| (row.get(6), row.get(5))).collect();
            let scopes: std::collections::HashMap<String, Vec<String>> = rows
                .iter()
                .map(|row| (row.get(0), decode(row.get(7))))
                .collect();
            let running_scopes: Vec<(i64, Vec<String>)> = rows.iter().filter(|row| row.get::<String, _>(5) == "running").map(|row| (row.get(2), decode(row.get(7)))).collect();
            let mut ready = Vec::with_capacity(rows.len());
            for row in rows {
                let candidate = ScheduledTask { task_id: row.get(0), phase: row.get(1), declaration_order: row.get(2), agent: row.get(3), dependencies: decode(row.get(4)), state: row.get(5) };
                let candidate_scope: Vec<String> = decode(row.get(7));
                let blocked_by_scope = running_scopes.iter().any(|(order, scope)| *order != candidate.declaration_order && *order < candidate.declaration_order && write_scope_conflicts(&candidate_scope, scope));
                let blocked_by_ready_scope = ready.iter().any(|earlier: &ScheduledTask| {
                    earlier.declaration_order < candidate.declaration_order
                        && scopes
                            .get(&earlier.task_id)
                            .is_some_and(|scope| write_scope_conflicts(&candidate_scope, scope))
                });
                if matches!(candidate.state.as_str(), "queued" | "ready") && candidate.dependencies.iter().all(|dependency| states.get(dependency).is_some_and(|state| state == "integrated")) && !blocked_by_scope && !blocked_by_ready_scope { ready.push(candidate); }
            }
            for task in &ready { sqlx::query("UPDATE workflow_tasks SET state='ready' WHERE task_id=? AND state='queued'").bind(&task.task_id).execute(&mut *tx).await?; }
            tx.commit().await?;
            Ok(ready)
        })
    }

    pub fn scheduled_tasks(
        &self,
        run_id: &str,
        phase_name: &str,
    ) -> Result<Vec<ScheduledTask>, FlowdexStoreError> {
        self.runtime.block_on(async {
            let rows = sqlx::query("SELECT task_id,phase,declaration_order,agent,dependencies,state FROM workflow_tasks WHERE run_id=? AND phase=? ORDER BY declaration_order").bind(run_id).bind(phase_name).fetch_all(&self.pool).await?;
            Ok(rows.into_iter().map(|row| ScheduledTask { task_id: row.get(0), phase: row.get(1), declaration_order: row.get(2), agent: row.get(3), dependencies: decode(row.get(4)), state: row.get(5) }).collect())
        })
    }

    pub fn next_integration_task(
        &self,
        run_id: &str,
        phase_name: &str,
    ) -> Result<Option<String>, FlowdexStoreError> {
        self.runtime.block_on(sqlx::query_scalar("SELECT task_id FROM workflow_tasks WHERE run_id=? AND phase=? AND state='verified' ORDER BY declaration_order LIMIT 1").bind(run_id).bind(phase_name).fetch_optional(&self.pool)).map_err(Into::into)
    }

    pub fn create_task(
        &self,
        run: &RunInfo,
        declaration: &TaskDeclaration,
    ) -> Result<TaskRecord, FlowdexStoreError> {
        self.ensure_run(run)?;
        let base_commit = git_stdout(&self.integration_worktree, ["rev-parse", "HEAD"])?;
        let task_dir = self.worktree_root.join(&run.run_id).join(&declaration.id);
        if task_dir.exists() {
            return Err(FlowdexStoreError::Integration(
                "task worktree already exists".to_string(),
            ));
        }
        if let Some(parent) = task_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut worktree_args = vec!["worktree".into(), "add".into(), "--detach".into()];
        worktree_args.push(task_dir.as_os_str().to_os_string());
        worktree_args.push("HEAD".into());
        git_status(&self.integration_worktree, worktree_args)?;
        let record = TaskRecord {
            id: declaration.id.clone(),
            run_id: run.run_id.clone(),
            name: declaration.name.clone(),
            instructions: declaration.instructions.clone(),
            read_scope: declaration.read_scope.clone(),
            write_scope: declaration.write_scope.clone(),
            verification: declaration.verification.clone(),
            base_commit,
            worktree_path: task_dir,
            state: "active".to_string(),
            last_verified_commit: None,
        };
        if let Err(error) = self.runtime.block_on(sqlx::query("INSERT INTO tasks(task_id,run_id,name,instructions,read_scope,write_scope,verification,base_commit,worktree_path,state,last_verified_commit) VALUES (?,?,?,?,?,?,?,?,?,?,NULL)")
            .bind(&record.id).bind(&record.run_id).bind(&record.name).bind(&record.instructions).bind(encode(&record.read_scope)).bind(encode(&record.write_scope)).bind(encode(&record.verification)).bind(&record.base_commit).bind(record.worktree_path.to_string_lossy().as_ref()).bind(&record.state).execute(&self.pool)) {
            remove_task_worktree(
                &self.integration_worktree,
                &self.worktree_root,
                &record.worktree_path,
            )?;
            return Err(error.into());
        }
        Ok(record)
    }

    pub fn task(&self, task_id: &str) -> Result<TaskRecord, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("SELECT task_id,run_id,name,instructions,read_scope,write_scope,verification,base_commit,worktree_path,state,last_verified_commit FROM tasks WHERE task_id=?").bind(task_id).fetch_optional(&self.pool))?.ok_or_else(|| FlowdexStoreError::TaskNotFound(task_id.to_string()))?;
        Ok(TaskRecord {
            id: row.get(0),
            run_id: row.get(1),
            name: row.get(2),
            instructions: row.get(3),
            read_scope: decode(row.get(4)),
            write_scope: decode(row.get(5)),
            verification: decode(row.get(6)),
            base_commit: row.get(7),
            worktree_path: PathBuf::from(row.get::<String, _>(8)),
            state: row.get(9),
            last_verified_commit: row.get(10),
        })
    }

    pub fn start_operation(
        &self,
        task_id: &str,
        operation_id: &str,
        agent_id: &str,
        model: &str,
    ) -> Result<TaskOperation, FlowdexStoreError> {
        let reservation_id = self.reserve_operation(task_id, model)?;
        self.bind_operation(task_id, &reservation_id, operation_id, agent_id)
    }

    pub fn reserve_operation(
        &self,
        task_id: &str,
        model: &str,
    ) -> Result<String, FlowdexStoreError> {
        let task = self.task(task_id)?;
        let active: Option<String> = self.runtime.block_on(sqlx::query_scalar("SELECT operation_id FROM task_operations WHERE task_id=? AND terminal_state IS NULL").bind(task_id).fetch_optional(&self.pool))?;
        if let Some(active) = active {
            return Err(FlowdexStoreError::Operation(format!(
                "operation already active: {active}"
            )));
        }
        let order: i64 = self.runtime.block_on(
            sqlx::query_scalar(
                "SELECT COALESCE(MAX(sequence),0)+1 FROM task_operations WHERE task_id=?",
            )
            .bind(task_id)
            .fetch_one(&self.pool),
        )?;
        let start_commit = git_stdout(&task.worktree_path, ["rev-parse", "HEAD"])?;
        let reservation_id = uuid::Uuid::new_v4().to_string();
        self.runtime.block_on(sqlx::query("INSERT INTO task_operations(operation_id,task_id,agent_id,model,start_commit,terminal_state,sequence) VALUES (?,?,'',?,?,NULL,?)").bind(&reservation_id).bind(task_id).bind(model).bind(&start_commit).bind(order).execute(&self.pool))?;
        Ok(reservation_id)
    }

    pub fn bind_operation(
        &self,
        task_id: &str,
        reservation_id: &str,
        operation_id: &str,
        agent_id: &str,
    ) -> Result<TaskOperation, FlowdexStoreError> {
        let row = self.runtime.block_on(sqlx::query("UPDATE task_operations SET operation_id=?,agent_id=? WHERE task_id=? AND operation_id=? AND terminal_state IS NULL RETURNING model,start_commit").bind(operation_id).bind(agent_id).bind(task_id).bind(reservation_id).fetch_optional(&self.pool))?.ok_or_else(|| FlowdexStoreError::Operation(reservation_id.to_string()))?;
        let model = row.get(0);
        let start_commit = row.get(1);
        Ok(TaskOperation {
            operation_id: operation_id.to_string(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            model,
            start_commit,
        })
    }

    pub fn cancel_operation_reservation(
        &self,
        task_id: &str,
        reservation_id: &str,
    ) -> Result<(), FlowdexStoreError> {
        let result = self.runtime.block_on(sqlx::query("DELETE FROM task_operations WHERE task_id=? AND operation_id=? AND agent_id='' AND terminal_state IS NULL").bind(task_id).bind(reservation_id).execute(&self.pool))?;
        if result.rows_affected() != 1 {
            return Err(FlowdexStoreError::Operation(reservation_id.to_string()));
        }
        Ok(())
    }

    pub fn finish_operation(
        &self,
        task_id: &str,
        operation_id: &str,
        terminal_state: &str,
    ) -> Result<Vec<TaskCommit>, FlowdexStoreError> {
        let task = self.task(task_id)?;
        let op = self.runtime.block_on(sqlx::query("SELECT agent_id,model,start_commit,sequence FROM task_operations WHERE task_id=? AND operation_id=? AND terminal_state IS NULL").bind(task_id).bind(operation_id).fetch_optional(&self.pool))?.ok_or_else(|| FlowdexStoreError::Operation(operation_id.to_string()))?;
        let agent_id: String = op.get(0);
        let model: String = op.get(1);
        let start_commit: String = op.get(2);
        let sequence: i64 = op.get(3);
        let head = git_stdout(&task.worktree_path, ["rev-parse", "HEAD"])?;
        let commits = if head == start_commit {
            Vec::new()
        } else {
            if git_status(
                &task.worktree_path,
                ["merge-base", "--is-ancestor", &start_commit, &head],
            )
            .is_err()
            {
                return Err(FlowdexStoreError::Operation(
                    "task history was rewritten".to_string(),
                ));
            }
            enumerate_commits(&task.worktree_path, &start_commit, &head)?
                .into_iter()
                .map(|(sha, summary)| TaskCommit {
                    source_commit: sha,
                    integrated_commit: None,
                    agent_id: agent_id.clone(),
                    model: model.clone(),
                    summary,
                })
                .collect()
        };
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            for (index, commit) in commits.iter().enumerate() { sqlx::query("INSERT INTO task_commits(task_id,source_commit,integrated_commit,operation_id,agent_id,model,sequence,summary) VALUES (?, ?, NULL, ?, ?, ?, ?, ?)").bind(task_id).bind(&commit.source_commit).bind(operation_id).bind(&agent_id).bind(&model).bind(sequence * 1_000_000 + index as i64).bind(&commit.summary).execute(&mut *tx).await?; }
            sqlx::query("UPDATE task_operations SET terminal_state=? WHERE operation_id=?").bind(terminal_state).bind(operation_id).execute(&mut *tx).await?;
            tx.commit().await
        })?;
        Ok(commits)
    }

    pub fn record_verification(
        &self,
        task_id: &str,
        verified_head: &str,
    ) -> Result<(), FlowdexStoreError> {
        let task = self.task(task_id)?;
        let actual = git_stdout(&task.worktree_path, ["rev-parse", "HEAD"])?;
        if actual != verified_head {
            return Err(FlowdexStoreError::Integration(
                "verification HEAD is stale".to_string(),
            ));
        }
        self.runtime.block_on(
            sqlx::query("UPDATE tasks SET last_verified_commit=? WHERE task_id=?")
                .bind(verified_head)
                .bind(task_id)
                .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn integrate(&self, task_id: &str) -> Result<IntegrationResult, FlowdexStoreError> {
        self.integrate_inner(task_id, true)
    }

    pub fn integrate_retained(
        &self,
        task_id: &str,
    ) -> Result<IntegrationResult, FlowdexStoreError> {
        self.integrate_inner(task_id, false)
    }

    fn integrate_inner(
        &self,
        task_id: &str,
        cleanup: bool,
    ) -> Result<IntegrationResult, FlowdexStoreError> {
        let task = self.task(task_id)?;
        if !worktree_clean(&task.worktree_path)? {
            return Err(FlowdexStoreError::Integration(
                "task worktree has uncommitted changes".to_string(),
            ));
        }
        if !task.verification.is_empty() {
            let verified = task.last_verified_commit.as_deref().ok_or_else(|| {
                FlowdexStoreError::Integration("task has not passed verification".to_string())
            })?;
            let head = git_stdout(&task.worktree_path, ["rev-parse", "HEAD"])?;
            if verified != head {
                return Err(FlowdexStoreError::Integration(
                    "task HEAD changed after verification".to_string(),
                ));
            }
        }
        self.runtime
            .block_on(self.integrate_locked(task_id, task, cleanup))
    }

    async fn integrate_locked(
        &self,
        task_id: &str,
        task: TaskRecord,
        cleanup: bool,
    ) -> Result<IntegrationResult, FlowdexStoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE integration_lock SET generation=generation+1 WHERE id=1")
            .execute(&mut *tx)
            .await?;
        if !worktree_clean(&self.integration_worktree)? {
            return Err(FlowdexStoreError::Integration(
                "integration worktree has uncommitted changes".to_string(),
            ));
        }
        if git_operation_in_progress(&self.integration_worktree)? {
            return Err(FlowdexStoreError::Integration(
                "integration worktree has a Git operation in progress".to_string(),
            ));
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM task_operations WHERE task_id=? AND terminal_state IS NULL",
        )
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
        if active != 0 {
            return Err(FlowdexStoreError::Integration(
                "task has an active operation".to_string(),
            ));
        }
        let incomplete: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_commits c JOIN task_operations o ON o.operation_id=c.operation_id WHERE c.task_id=? AND o.terminal_state != 'completed'").bind(task_id).fetch_one(&mut *tx).await?;
        if incomplete != 0 {
            return Err(FlowdexStoreError::Integration(
                "task has commits from an incomplete operation".to_string(),
            ));
        }
        let rows = sqlx::query("SELECT source_commit,integrated_commit,agent_id,model,summary FROM task_commits WHERE task_id=? ORDER BY sequence").bind(task_id).fetch_all(&mut *tx).await?;
        let source: Vec<TaskCommit> = rows
            .into_iter()
            .map(|row| TaskCommit {
                source_commit: row.get(0),
                integrated_commit: row.get(1),
                agent_id: row.get(2),
                model: row.get(3),
                summary: row.get(4),
            })
            .collect();
        let task_head = git_stdout(&task.worktree_path, ["rev-parse", "HEAD"])?;
        let history = enumerate_commits(&task.worktree_path, &task.base_commit, &task_head)?;
        if history.len() != source.len()
            || history
                .iter()
                .zip(&source)
                .any(|((commit, _), attributed)| commit != &attributed.source_commit)
        {
            return Err(FlowdexStoreError::Integration(
                "task history does not exactly match attributed commits".to_string(),
            ));
        }
        let pre_head = git_stdout(&self.integration_worktree, ["rev-parse", "HEAD"])?;
        let mut result_commits = Vec::with_capacity(source.len());
        for commit in source
            .iter()
            .filter(|commit| commit.integrated_commit.is_none())
        {
            if let Err(error) = git_status(
                &self.integration_worktree,
                [
                    "cherry-pick",
                    "--keep-redundant-commits",
                    &commit.source_commit,
                ],
            ) {
                rollback_integration(&self.integration_worktree, &pre_head)?;
                return Err(FlowdexStoreError::Integration(error.to_string()));
            }
            let integrated = git_stdout(&self.integration_worktree, ["rev-parse", "HEAD"])?;
            result_commits.push(TaskCommit {
                integrated_commit: Some(integrated),
                ..commit.clone()
            });
        }
        for commit in &result_commits {
            sqlx::query(
                "UPDATE task_commits SET integrated_commit=? WHERE task_id=? AND source_commit=?",
            )
            .bind(commit.integrated_commit.as_deref())
            .bind(task_id)
            .bind(&commit.source_commit)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE tasks SET state='integrated' WHERE task_id=?")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workflow_tasks SET state='integrated' WHERE task_id=?")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        if cleanup {
            if let Err(error) = remove_task_worktree(
                &self.integration_worktree,
                &self.worktree_root,
                &task.worktree_path,
            ) {
                rollback_integration(&self.integration_worktree, &pre_head)?;
                return Err(error);
            }
        }
        tx.commit().await?;
        Ok(IntegrationResult {
            task_id: task_id.to_string(),
            commits: result_commits,
        })
    }

    pub fn cleanup_task_worktree(&self, task_id: &str) -> Result<(), FlowdexStoreError> {
        let task = self.task(task_id)?;
        remove_task_worktree(
            &self.integration_worktree,
            &self.worktree_root,
            &task.worktree_path,
        )
    }

    pub fn task_commit_operation(
        &self,
        task_id: &str,
        source_commit: &str,
    ) -> Result<String, FlowdexStoreError> {
        self.runtime
            .block_on(
                sqlx::query_scalar(
                    "SELECT operation_id FROM task_commits WHERE task_id=? AND source_commit=?",
                )
                .bind(task_id)
                .bind(source_commit)
                .fetch_optional(&self.pool),
            )?
            .ok_or_else(|| {
                FlowdexStoreError::Integration(format!(
                    "task commit not found: {task_id}:{source_commit}"
                ))
            })
    }
}

impl Drop for FlowdexStore {
    fn drop(&mut self) {
        // Tokio runtimes must not perform their blocking shutdown inside async code.
        let runtime = unsafe { ManuallyDrop::take(&mut self.runtime) };
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::spawn(move || drop(runtime));
        } else {
            drop(runtime);
        }
    }
}

async fn insert_schedule_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    run_id: &str,
    phase: &str,
    phase_instructions: &str,
    declaration_order: i64,
    task: &TaskDefinition,
    state: SchedulerTaskState,
) -> Result<(), sqlx::Error> {
    let instructions = format!("{phase_instructions}\n\n{}", task.instructions);
    sqlx::query("INSERT INTO workflow_tasks(task_id,run_id,phase,name,instructions,declaration_order,agent,dependencies,read_scope,write_scope,verification,state,verification_repair_limit,boundary,review_agent,review_instructions,review_max_rounds) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(task_id).bind(run_id).bind(phase).bind(&task.name).bind(instructions).bind(declaration_order).bind(&task.agent)
        .bind(encode(&task.dependencies)).bind(encode(&task.read_scope)).bind(encode(&task.write_scope)).bind(encode(&task.verification)).bind(state.as_str()).bind(task.verification_repair_limit as i64).bind(boundary_name(task.boundary)).bind(task.review.as_ref().map(|review| review.agent.as_str())).bind(task.review.as_ref().map(|review| review.instructions.as_str())).bind(task.review.as_ref().map(|review| review.max_rounds as i64))
        .execute(&mut **tx).await?;
    Ok(())
}

async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let statements = "PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS repository(identity TEXT NOT NULL PRIMARY KEY);
      CREATE TABLE IF NOT EXISTS runs(run_id TEXT PRIMARY KEY,parent_thread_id TEXT NOT NULL,workflow_path TEXT NOT NULL,parent_run_id TEXT,workflow_identity TEXT,repository_identity TEXT NOT NULL,integration_worktree TEXT NOT NULL,created_at INTEGER NOT NULL,name TEXT NOT NULL DEFAULT '',verification TEXT NOT NULL DEFAULT '[]',state TEXT NOT NULL DEFAULT 'queued',verification_repair_count INTEGER NOT NULL DEFAULT 0,review_round_count INTEGER NOT NULL DEFAULT 0,boundary TEXT NOT NULL DEFAULT 'continue');
      CREATE TABLE IF NOT EXISTS tasks(task_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,name TEXT NOT NULL,instructions TEXT NOT NULL,read_scope TEXT NOT NULL,write_scope TEXT NOT NULL,verification TEXT NOT NULL,base_commit TEXT NOT NULL,worktree_path TEXT NOT NULL,state TEXT NOT NULL,last_verified_commit TEXT);
      CREATE TABLE IF NOT EXISTS workflow_phases(run_id TEXT NOT NULL,name TEXT NOT NULL,declaration_order INTEGER NOT NULL,instructions TEXT NOT NULL,open INTEGER NOT NULL,sealed INTEGER NOT NULL,verification TEXT NOT NULL,state TEXT NOT NULL,verification_repair_count INTEGER NOT NULL DEFAULT 0,review_round_count INTEGER NOT NULL DEFAULT 0,boundary TEXT NOT NULL DEFAULT 'continue',review_agent TEXT,review_instructions TEXT,review_max_rounds INTEGER,PRIMARY KEY(run_id,name));
      CREATE TABLE IF NOT EXISTS workflow_agents(run_id TEXT NOT NULL,name TEXT NOT NULL,profile TEXT,model TEXT,reasoning_effort TEXT,tool_profile TEXT,PRIMARY KEY(run_id,name));
      CREATE TABLE IF NOT EXISTS workflow_tasks(task_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,phase TEXT NOT NULL,name TEXT NOT NULL,instructions TEXT NOT NULL DEFAULT '',declaration_order INTEGER NOT NULL,agent TEXT NOT NULL,dependencies TEXT NOT NULL,read_scope TEXT NOT NULL,write_scope TEXT NOT NULL,verification TEXT NOT NULL,state TEXT NOT NULL,verification_repair_limit INTEGER NOT NULL DEFAULT 0,verification_repair_count INTEGER NOT NULL DEFAULT 0,review_round_count INTEGER NOT NULL DEFAULT 0,boundary TEXT NOT NULL DEFAULT 'continue',review_agent TEXT,review_instructions TEXT,review_max_rounds INTEGER,UNIQUE(run_id,phase,name));
      CREATE TABLE IF NOT EXISTS task_operations(operation_id TEXT PRIMARY KEY,task_id TEXT NOT NULL,agent_id TEXT NOT NULL,model TEXT NOT NULL,start_commit TEXT NOT NULL,terminal_state TEXT,sequence INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS task_commits(task_id TEXT NOT NULL,source_commit TEXT NOT NULL,integrated_commit TEXT,operation_id TEXT NOT NULL,agent_id TEXT NOT NULL,model TEXT NOT NULL,sequence INTEGER NOT NULL,summary TEXT NOT NULL,PRIMARY KEY(task_id,source_commit));
      CREATE TABLE IF NOT EXISTS integration_lock(id INTEGER PRIMARY KEY CHECK(id=1),generation INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS context_packs(run_id TEXT NOT NULL,name TEXT NOT NULL,agent TEXT NOT NULL,instructions TEXT NOT NULL,PRIMARY KEY(run_id,name));
      CREATE TABLE IF NOT EXISTS context_fragments(run_id TEXT NOT NULL,pack TEXT NOT NULL,fragment_key TEXT NOT NULL,version INTEGER NOT NULL,publisher_thread_id TEXT,publisher_agent_id TEXT,path TEXT NOT NULL,line_start INTEGER NOT NULL,line_end INTEGER NOT NULL,summary TEXT,content TEXT NOT NULL,content_hash TEXT NOT NULL,superseded_version INTEGER,created_at INTEGER NOT NULL,PRIMARY KEY(run_id,pack,fragment_key,version));
      CREATE TABLE IF NOT EXISTS review_operations(operation_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,scope_kind TEXT NOT NULL,scope_id TEXT NOT NULL,round INTEGER NOT NULL,reviewer_thread_id TEXT NOT NULL,state TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS review_findings(finding_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL,finding_order INTEGER NOT NULL,file TEXT NOT NULL,line_start INTEGER NOT NULL,line_end INTEGER NOT NULL,reason TEXT NOT NULL,rule_key TEXT,ast_grep_suitable INTEGER NOT NULL,attributed_task_id TEXT,attributed_operation_id TEXT,attributed_agent_id TEXT);
      CREATE TABLE IF NOT EXISTS review_resolutions(finding_id TEXT NOT NULL,repair_operation_id TEXT NOT NULL,source_commit TEXT NOT NULL,integrated_commit TEXT,PRIMARY KEY(finding_id,repair_operation_id,source_commit));
      CREATE TABLE IF NOT EXISTS pending_boundaries(run_id TEXT NOT NULL,scope_kind TEXT NOT NULL,scope_id TEXT NOT NULL,target TEXT NOT NULL,reason TEXT NOT NULL,transition TEXT NOT NULL,PRIMARY KEY(run_id,scope_kind,scope_id));
      CREATE TABLE IF NOT EXISTS pending_signals(signal_id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL,signal TEXT NOT NULL);
      INSERT OR IGNORE INTO integration_lock(id,generation) VALUES(1,0);";
    for statement in statements
        .split(';')
        .filter(|statement| !statement.trim().is_empty())
    {
        sqlx::query(statement).execute(pool).await?;
    }
    // Existing Batch 009 databases predate run name/state columns.
    for statement in [
        "ALTER TABLE runs ADD COLUMN name TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE runs ADD COLUMN state TEXT NOT NULL DEFAULT 'queued'",
        "ALTER TABLE runs ADD COLUMN verification TEXT NOT NULL DEFAULT '[]'",
        "ALTER TABLE runs ADD COLUMN parent_run_id TEXT",
        "ALTER TABLE runs ADD COLUMN workflow_identity TEXT",
        "ALTER TABLE workflow_tasks ADD COLUMN instructions TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE runs ADD COLUMN verification_repair_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE runs ADD COLUMN review_round_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE runs ADD COLUMN boundary TEXT NOT NULL DEFAULT 'continue'",
        "ALTER TABLE workflow_phases ADD COLUMN verification_repair_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE workflow_phases ADD COLUMN review_round_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE workflow_phases ADD COLUMN boundary TEXT NOT NULL DEFAULT 'continue'",
        "ALTER TABLE workflow_phases ADD COLUMN review_agent TEXT",
        "ALTER TABLE workflow_phases ADD COLUMN review_instructions TEXT",
        "ALTER TABLE workflow_phases ADD COLUMN review_max_rounds INTEGER",
        "ALTER TABLE workflow_tasks ADD COLUMN verification_repair_limit INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE workflow_tasks ADD COLUMN verification_repair_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE workflow_tasks ADD COLUMN review_round_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE workflow_tasks ADD COLUMN boundary TEXT NOT NULL DEFAULT 'continue'",
        "ALTER TABLE workflow_tasks ADD COLUMN review_agent TEXT",
        "ALTER TABLE workflow_tasks ADD COLUMN review_instructions TEXT",
        "ALTER TABLE workflow_tasks ADD COLUMN review_max_rounds INTEGER",
        "ALTER TABLE workflow_agents ADD COLUMN tool_profile TEXT",
    ] {
        let _ = sqlx::query(statement).execute(pool).await;
    }
    Ok(())
}

fn encode(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}
fn boundary_name(boundary: crate::workflow::Boundary) -> &'static str {
    match boundary {
        crate::workflow::Boundary::Continue => "continue",
        crate::workflow::Boundary::Orchestrator => "orchestrator",
        crate::workflow::Boundary::Human => "human",
    }
}
fn decode(value: &str) -> Vec<String> {
    serde_json::from_str(value).unwrap_or_default()
}
fn repository_key(identity: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(identity.as_bytes());
    digest.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn hash_content(content: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(content.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn git_status<I, S>(dir: &Path, args: I) -> Result<(), FlowdexStoreError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_git(dir, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(FlowdexStoreError::Git(format_git_error(&output)))
    }
}
fn worktree_clean(dir: &Path) -> Result<bool, FlowdexStoreError> {
    Ok(git_stdout(dir, ["status", "--porcelain", "--untracked-files=all"])?.is_empty())
}
fn git_stdout<I, S>(dir: &Path, args: I) -> Result<String, FlowdexStoreError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_git(dir, args)?;
    if !output.status.success() {
        return Err(FlowdexStoreError::Git(format_git_error(&output)));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|_| FlowdexStoreError::Git("git returned invalid UTF-8".to_string()))
}

fn commit_diff(
    worktree: &Path,
    commit: &str,
    max_bytes: usize,
) -> Result<String, FlowdexStoreError> {
    let output = run_git(
        worktree,
        [
            "diff-tree",
            "--root",
            "--binary",
            "--no-ext-diff",
            "--no-commit-id",
            "-p",
            commit,
        ],
    )?;
    if !output.status.success() {
        return Err(FlowdexStoreError::Git(format_git_error(&output)));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| FlowdexStoreError::Git("git returned invalid UTF-8".to_string()))?;
    Ok(bound_text(text, max_bytes))
}
fn run_git<I, S>(dir: &Path, args: I) -> Result<Output, FlowdexStoreError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git").current_dir(dir).args(args).output()?;
    Ok(output)
}
fn format_git_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = bound_git_output(stderr.trim().to_string());
    if text.is_empty() {
        format!("exit status {}", output.status)
    } else {
        text
    }
}

fn bound_git_output(text: String) -> String {
    bound_text(text, MAX_GIT_OUTPUT)
}

fn bound_text(mut text: String, max_bytes: usize) -> String {
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
fn enumerate_commits(
    worktree: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<(String, String)>, FlowdexStoreError> {
    let text = git_stdout(
        worktree,
        [
            "log",
            "--reverse",
            "--format=%H%x00%P%x00%s",
            &format!("{base}..{head}"),
        ],
    )?;
    let mut commits = Vec::new();
    for line in text.lines() {
        let mut fields = line.splitn(3, '\0');
        let sha = fields.next().unwrap_or_default();
        let parents = fields.next().unwrap_or_default();
        let summary = fields.next().unwrap_or_default().to_string();
        if sha.is_empty() || parents.split_whitespace().count() != 1 {
            return Err(FlowdexStoreError::Operation(
                "task history contains a merge commit or rewrite".to_string(),
            ));
        }
        commits.push((sha.to_string(), summary));
    }
    Ok(commits)
}
fn remove_task_worktree(
    integration: &Path,
    root: &Path,
    path: &Path,
) -> Result<(), FlowdexStoreError> {
    let canonical_root = fs::canonicalize(root)?;
    let canonical_path = fs::canonicalize(path)?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(FlowdexStoreError::Integration(
            "refusing to remove worktree outside Flowdex root".to_string(),
        ));
    }
    let registered = git_stdout(integration, ["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .filter_map(|listed| fs::canonicalize(listed).ok())
        .any(|listed| listed == canonical_path);
    if !registered {
        return Err(FlowdexStoreError::Integration(
            "refusing to remove an unregistered task worktree".to_string(),
        ));
    }
    let mut args = vec!["worktree".into(), "remove".into(), "--force".into()];
    args.push(path.as_os_str().to_os_string());
    git_status(integration, args)
}

fn git_operation_in_progress(worktree: &Path) -> Result<bool, FlowdexStoreError> {
    for state in [
        "CHERRY_PICK_HEAD",
        "MERGE_HEAD",
        "REBASE_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
    ] {
        let path = git_stdout(worktree, ["rev-parse", "--git-path", state])?;
        let path = PathBuf::from(path);
        if (if path.is_absolute() {
            path
        } else {
            worktree.join(path)
        })
        .exists()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rollback_integration(worktree: &Path, pre_head: &str) -> Result<(), FlowdexStoreError> {
    if git_operation_in_progress(worktree)? {
        let _ = git_status(worktree, ["cherry-pick", "--abort"]);
    }
    git_status(worktree, ["reset", "--hard", pre_head])?;
    let head = git_stdout(worktree, ["rev-parse", "HEAD"])?;
    if head != pre_head || !worktree_clean(worktree)? || git_operation_in_progress(worktree)? {
        return Err(FlowdexStoreError::Integration(
            "integration rollback did not restore the original Git state".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{AgentDefinition, PhaseDefinition, TaskDefinition, WorkflowDefinition};
    use codex_utils_absolute_path::AbsolutePathBuf;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git")
                .status
                .success()
        );
    }
    fn store() -> (tempfile::TempDir, tempfile::TempDir, FlowdexStore, RunInfo) {
        let repo = tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        git(
            repo.path(),
            &["config", "user.email", "flowdex@example.com"],
        );
        git(repo.path(), &["config", "user.name", "Flowdex"]);
        fs::write(repo.path().join("README"), "base").unwrap();
        git(repo.path(), &["add", "README"]);
        git(repo.path(), &["commit", "-qm", "base"]);
        let home = tempdir().unwrap();
        let root = AbsolutePathBuf::from_absolute_path(repo.path()).unwrap();
        let store =
            FlowdexStore::open(home.path(), root.to_string_lossy().to_string(), repo.path())
                .unwrap();
        let run = RunInfo {
            run_id: "run".into(),
            parent_thread_id: "thread".into(),
            workflow_path: ".flowdex/workflows/a.js".into(),
            parent_run_id: None,
            workflow_identity: None,
            repository_identity: root.to_string_lossy().to_string(),
            integration_worktree: repo.path().into(),
        };
        (repo, home, store, run)
    }

    #[test]
    fn derives_rule_candidates_from_resolved_history() {
        let (_repo, _home, store, _run) = store();
        for index in 0..4 {
            let finding_id = format!("finding-{index}");
            store.runtime.block_on(sqlx::query("INSERT INTO review_findings(finding_id,operation_id,finding_order,file,line_start,line_end,reason,rule_key,ast_grep_suitable) VALUES (?,?,?,?,?,?,?,?,?)")
                .bind(&finding_id).bind("review").bind(index).bind(format!("src/{index}.rs")).bind(10 + index).bind(10 + index).bind("reason").bind("avoid-cast").bind(1_i64).execute(&store.pool)).unwrap();
            store.runtime.block_on(sqlx::query("INSERT INTO task_commits(task_id,source_commit,integrated_commit,operation_id,agent_id,model,sequence,summary) VALUES (?,?,?,?,?,?,?,?)")
                .bind(format!("task-{index}")).bind(format!("source-{index}")).bind(format!("integrated-{index}")).bind(format!("repair-{index}")).bind("agent").bind("model").bind(index).bind("summary").execute(&store.pool)).unwrap();
            store.runtime.block_on(sqlx::query("INSERT INTO review_resolutions(finding_id,repair_operation_id,source_commit,integrated_commit) VALUES (?,?,?,NULL)")
                .bind(&finding_id).bind(format!("repair-{index}")).bind(format!("source-{index}")).execute(&store.pool)).unwrap();
        }
        let candidates = store
            .rule_candidates(3, &std::collections::BTreeSet::new())
            .expect("candidate scan");
        assert_eq!(candidates.candidates.len(), 1);
        assert_eq!(candidates.candidates[0].resolved_occurrences, 4);
        assert_eq!(candidates.candidates[0].examples.len(), 3);
    }

    #[test]
    fn candidate_scan_uses_final_repair_and_excludes_ineligible_rows() {
        let (_repo, _home, store, _run) = store();
        for (finding_id, suitable, rule_key) in [
            ("final", 1_i64, Some("stable")),
            ("unsuitable", 0, Some("stable")),
            ("blank", 1, Some(" ")),
            ("unresolved", 1, Some("stable")),
        ] {
            store.runtime.block_on(sqlx::query("INSERT INTO review_findings(finding_id,operation_id,finding_order,file,line_start,line_end,reason,rule_key,ast_grep_suitable) VALUES (?,?,?,?,?,?,?,?,?)")
                .bind(finding_id).bind("review").bind(0_i64).bind(format!("{finding_id}.rs")).bind(1_i64).bind(1_i64).bind("reason").bind(rule_key).bind(suitable).execute(&store.pool)).unwrap();
        }
        for (source, integrated, operation, sequence) in [
            ("old-source", "old-integrated", "repair-old", 1_i64),
            ("new-source", "new-integrated", "repair-new", 2_i64),
        ] {
            store.runtime.block_on(sqlx::query("INSERT INTO task_commits(task_id,source_commit,integrated_commit,operation_id,agent_id,model,sequence,summary) VALUES (?,?,?,?,?,?,?,?)")
                .bind(operation).bind(source).bind(integrated).bind(operation).bind("agent").bind("model").bind(sequence).bind("summary").execute(&store.pool)).unwrap();
            store.runtime.block_on(sqlx::query("INSERT INTO review_resolutions(finding_id,repair_operation_id,source_commit,integrated_commit) VALUES ('final',?,?,NULL)")
                .bind(operation).bind(source).execute(&store.pool)).unwrap();
        }
        // A mismatched source commit must not inherit a task commit's integration.
        store.runtime.block_on(sqlx::query("INSERT INTO review_resolutions(finding_id,repair_operation_id,source_commit,integrated_commit) VALUES ('unresolved','repair-old','wrong-source',NULL)").execute(&store.pool)).unwrap();
        let candidates = store
            .rule_candidates(1, &std::collections::BTreeSet::new())
            .expect("candidate scan");
        assert_eq!(candidates.candidates.len(), 1);
        assert_eq!(candidates.candidates[0].resolved_occurrences, 1);
        assert_eq!(
            candidates.candidates[0].examples[0].source_commit,
            "new-source"
        );
        assert_eq!(
            candidates.candidates[0].examples[0].integrated_commit,
            "new-integrated"
        );
    }

    #[test]
    fn candidate_scan_excludes_approved_ids_and_caps_groups_at_fifty() {
        let (_repo, _home, store, _run) = store();
        for index in 0..51_i64 {
            let finding_id = format!("finding-{index}");
            let rule_key = format!("rule-{index:02}");
            let operation = format!("repair-{index}");
            let source = format!("source-{index}");
            store.runtime.block_on(sqlx::query("INSERT INTO review_findings(finding_id,operation_id,finding_order,file,line_start,line_end,reason,rule_key,ast_grep_suitable) VALUES (?,?,?,?,?,?,?,?,1)")
                .bind(&finding_id).bind("review").bind(index).bind(format!("src/{index}.rs")).bind(1_i64).bind(1_i64).bind("reason").bind(&rule_key).execute(&store.pool)).unwrap();
            store.runtime.block_on(sqlx::query("INSERT INTO task_commits(task_id,source_commit,integrated_commit,operation_id,agent_id,model,sequence,summary) VALUES (?,?,?,?,?,?,?,?)")
                .bind(&operation).bind(&source).bind(format!("integrated-{index}")).bind(&operation).bind("agent").bind("model").bind(index).bind("summary").execute(&store.pool)).unwrap();
            store.runtime.block_on(sqlx::query("INSERT INTO review_resolutions(finding_id,repair_operation_id,source_commit,integrated_commit) VALUES (?,?,?,NULL)")
                .bind(&finding_id).bind(&operation).bind(&source).execute(&store.pool)).unwrap();
        }
        let approved = std::collections::BTreeSet::from(["rule-00".to_string()]);
        let candidates = store.rule_candidates(1, &approved).expect("candidate scan");
        assert_eq!(candidates.candidates.len(), 50);
        assert!(
            candidates
                .candidates
                .iter()
                .all(|candidate| candidate.rule_key != "rule-00")
        );
        assert_eq!(candidates.candidates.first().unwrap().rule_key, "rule-01");
    }

    #[test]
    fn lifecycle_create_commit_integrate() {
        let (repo, _home, store, run) = store();
        let task = store
            .create_task(
                &run,
                &TaskDeclaration {
                    id: "task".into(),
                    name: "n".into(),
                    instructions: "i".into(),
                    read_scope: vec![],
                    write_scope: vec![],
                    verification: vec![],
                },
            )
            .unwrap();
        let _ = store
            .start_operation("task", "op", "agent", "model")
            .unwrap();
        fs::write(task.worktree_path.join("x"), "change").unwrap();
        git(&task.worktree_path, &["add", "x"]);
        git(&task.worktree_path, &["commit", "-qm", "first"]);
        fs::write(task.worktree_path.join("y"), "change").unwrap();
        git(&task.worktree_path, &["add", "y"]);
        git(&task.worktree_path, &["commit", "-qm", "second"]);
        let commits = store.finish_operation("task", "op", "completed").unwrap();
        assert_eq!(commits.len(), 2);
        let result = store.integrate("task").unwrap();
        assert_eq!(result.commits.len(), 2);
        assert_ne!(
            result.commits[0].integrated_commit,
            result.commits[1].integrated_commit
        );
        assert!(!task.worktree_path.exists());
        assert!(repo.path().join("x").exists());
        assert!(repo.path().join("y").exists());
    }

    #[test]
    fn committed_diff_and_review_attribution_are_exact() {
        let (repo, _home, store, run) = store();
        let task = store
            .create_task(
                &run,
                &TaskDeclaration {
                    id: "task".into(),
                    name: "n".into(),
                    instructions: "i".into(),
                    read_scope: vec![],
                    write_scope: vec![],
                    verification: vec![],
                },
            )
            .unwrap();
        store
            .start_operation("task", "op", "agent", "model")
            .unwrap();
        fs::write(task.worktree_path.join("reviewed.txt"), "reviewed\n").unwrap();
        git(&task.worktree_path, &["add", "reviewed.txt"]);
        git(&task.worktree_path, &["commit", "-qm", "reviewed"]);
        let source = store.finish_operation("task", "op", "completed").unwrap();
        let diffs = store.committed_diffs(repo.path(), &source, 4096).unwrap();
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].patch.contains("reviewed.txt"));
        let integrated = store.integrate("task").unwrap().commits.remove(0);
        store
            .record_review_operation(&ReviewOperation {
                operation_id: "review".into(),
                run_id: run.run_id.clone(),
                scope_kind: "task".into(),
                scope_id: "task".into(),
                round: 1,
                reviewer_thread_id: "reviewer".into(),
                state: "reported".into(),
            })
            .unwrap();
        store
            .record_review_findings(&[ReviewFinding {
                finding_id: "finding".into(),
                operation_id: "review".into(),
                finding_order: 0,
                file: "reviewed.txt".into(),
                line_start: 1,
                line_end: 1,
                reason: "bad line".into(),
                rule_key: None,
                ast_grep_suitable: false,
                attributed_task_id: None,
                attributed_operation_id: None,
                attributed_agent_id: None,
            }])
            .unwrap();
        let attribution = store
            .attribute_review_finding(
                "finding",
                repo.path(),
                integrated.integrated_commit.as_deref().unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(attribution.source_commit, source[0].source_commit);
        assert_eq!(attribution.operation_id, "op");
        store
            .record_review_resolution(&ReviewResolution {
                finding_id: "finding".into(),
                repair_operation_id: "op".into(),
                source_commit: source[0].source_commit.clone(),
                integrated_commit: integrated.integrated_commit,
            })
            .unwrap();
        assert_eq!(store.review_resolutions("finding").unwrap().len(), 1);
    }

    #[test]
    fn reservation_captures_head_before_operation_is_bound() {
        let (_repo, _home, store, run) = store();
        let task = store
            .create_task(
                &run,
                &TaskDeclaration {
                    id: "task".into(),
                    name: "n".into(),
                    instructions: "i".into(),
                    read_scope: vec![],
                    write_scope: vec![],
                    verification: vec![],
                },
            )
            .unwrap();
        let reservation = store.reserve_operation("task", "model").unwrap();
        fs::write(task.worktree_path.join("fast"), "commit before bind").unwrap();
        git(&task.worktree_path, &["add", "fast"]);
        git(&task.worktree_path, &["commit", "-qm", "fast"]);
        let operation = store
            .bind_operation("task", &reservation, "exact-op", "agent")
            .unwrap();
        assert_eq!(operation.operation_id, "exact-op");
        let commits = store
            .finish_operation("task", "exact-op", "completed")
            .unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].summary, "fast");
        assert_eq!(commits[0].agent_id, "agent");
    }
    #[test]
    fn conflict_preserves_task_and_rolls_back() {
        let (repo, _home, store, run) = store();
        let task = store
            .create_task(
                &run,
                &TaskDeclaration {
                    id: "task".into(),
                    name: "n".into(),
                    instructions: "i".into(),
                    read_scope: vec![],
                    write_scope: vec![],
                    verification: vec![],
                },
            )
            .unwrap();
        let _ = store
            .start_operation("task", "op", "agent", "model")
            .unwrap();
        fs::write(task.worktree_path.join("README"), "task").unwrap();
        git(&task.worktree_path, &["add", "README"]);
        git(&task.worktree_path, &["commit", "-qm", "task"]);
        store.finish_operation("task", "op", "completed").unwrap();
        fs::write(repo.path().join("README"), "integration").unwrap();
        git(repo.path(), &["add", "README"]);
        git(repo.path(), &["commit", "-qm", "integration"]);
        let before = git_stdout(repo.path(), ["rev-parse", "HEAD"]).unwrap();
        assert!(store.integrate("task").is_err());
        assert_eq!(
            before,
            git_stdout(repo.path(), ["rev-parse", "HEAD"]).unwrap()
        );
        assert!(task.worktree_path.exists());
    }

    #[test]
    fn integration_rejects_active_and_unattributed_history() {
        let (_repo, _home, store, run) = store();
        let task = store
            .create_task(
                &run,
                &TaskDeclaration {
                    id: "task".into(),
                    name: "n".into(),
                    instructions: "i".into(),
                    read_scope: vec![],
                    write_scope: vec![],
                    verification: vec![],
                },
            )
            .unwrap();
        store
            .start_operation("task", "op", "agent", "model")
            .unwrap();
        assert!(store.integrate("task").is_err());
        store.finish_operation("task", "op", "completed").unwrap();
        fs::write(task.worktree_path.join("unattributed"), "change").unwrap();
        git(&task.worktree_path, &["add", "unattributed"]);
        git(&task.worktree_path, &["commit", "-qm", "unattributed"]);
        assert!(store.integrate("task").is_err());
    }

    #[test]
    fn metadata_failure_removes_created_worktree() {
        let (_repo, _home, store, run) = store();
        let declaration = TaskDeclaration {
            id: "task".into(),
            name: "n".into(),
            instructions: "i".into(),
            read_scope: vec![],
            write_scope: vec![],
            verification: vec![],
        };
        store.create_task(&run, &declaration).unwrap();
        let mut second_run = run;
        second_run.run_id = "second".into();
        assert!(store.create_task(&second_run, &declaration).is_err());
        assert!(!store.worktree_root.join("second").join("task").exists());
    }

    #[test]
    fn git_output_bound_preserves_utf8() {
        let text = "a".repeat(MAX_GIT_OUTPUT - 1) + "é";
        let bounded = bound_git_output(text);
        assert_eq!(bounded.len(), MAX_GIT_OUTPUT - 1);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn scheduler_getters_and_transitions_are_durable() {
        let (_repo, _home, store, run) = store();
        let definition = WorkflowDefinition {
            name: "workflow".into(),
            agents: [(
                "worker".into(),
                AgentDefinition {
                    profile: Some("implementation_worker".into()),
                    model: None,
                    reasoning_effort: None,
                    tool_profile: Some("research".into()),
                },
            )]
            .into_iter()
            .collect(),
            verification: vec!["git diff --check".into()],
            boundary: crate::workflow::Boundary::Continue,
            context_packs: BTreeMap::new(),
            phases: vec![PhaseDefinition {
                name: "phase".into(),
                instructions: "phase instructions".into(),
                open: false,
                verification: vec!["cargo test".into()],
                boundary: crate::workflow::Boundary::Continue,
                review: None,
                tasks: vec![TaskDefinition {
                    name: "task".into(),
                    agent: "worker".into(),
                    instructions: "task instructions".into(),
                    dependencies: vec![],
                    read_scope: vec!["src/**".into()],
                    write_scope: vec!["src/**".into()],
                    verification: vec![],
                    verification_repair_limit: 0,
                    review: None,
                    boundary: crate::workflow::Boundary::Continue,
                    context: vec![],
                }],
            }],
        };
        store.initialize_workflow(&run, &definition).unwrap();
        let task_id = store.scheduled_tasks("run", "phase").unwrap()[0]
            .task_id
            .clone();
        let task = store.scheduler_task(&task_id).unwrap();
        assert_eq!(task.instructions, "phase instructions\n\ntask instructions");
        assert_eq!(task.agent, "worker");
        assert_eq!(
            store
                .workflow_agent("run", "worker")
                .unwrap()
                .profile
                .as_deref(),
            Some("implementation_worker")
        );
        assert_eq!(
            store
                .workflow_agent("run", "worker")
                .unwrap()
                .tool_profile
                .as_deref(),
            Some("research")
        );
        assert_eq!(
            store.run_metadata("run").unwrap().verification,
            vec!["git diff --check"]
        );
        assert_eq!(store.phase_metadata("run", "phase").unwrap().total, 1);
        store.mark_run_running("run").unwrap();
        store.mark_phase_running("run", "phase").unwrap();
        store.mark_task_running(&task_id).unwrap();
        store.mark_task_attributing(&task_id).unwrap();
        store.mark_task_integrated(&task_id).unwrap();
        store.mark_phase_completed("run", "phase").unwrap();
        store.mark_run_completed("run").unwrap();
        assert_eq!(
            store
                .increment_review_rounds("phase", "run", "phase")
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .increment_verification_repairs("task", "run", &task_id)
                .unwrap(),
            1
        );
        assert_eq!(store.scheduler_task(&task_id).unwrap().state, "integrated");
        assert_eq!(store.run_metadata("run").unwrap().state, "completed");
    }

    #[test]
    fn pending_signals_are_fifo_and_independently_consumed() {
        let (_repo, _home, store, _run) = store();
        assert!(store.enqueue_signal("run", "  ").is_err());
        store.enqueue_signal("run", "build-complete").unwrap();
        store.enqueue_signal("run", "build-complete").unwrap();
        store.enqueue_signal("run", "deploy-complete").unwrap();

        let first = store.oldest_pending_signal("run").unwrap().unwrap();
        let first_id = first.id;
        assert_eq!(first.signal, "build-complete");
        store.consume_signal("run", first.id).unwrap();
        store
            .restore_signal("run", first.id, &first.signal)
            .unwrap();
        let restored = store.oldest_pending_signal("run").unwrap().unwrap();
        assert_eq!(restored.id, first_id);
        store.consume_signal("run", restored.id).unwrap();
        let second = store.oldest_pending_signal("run").unwrap().unwrap();
        assert_ne!(second.id, first_id);
        assert_eq!(second.signal, "build-complete");
        store.consume_signal("run", second.id).unwrap();
        let third = store.oldest_pending_signal("run").unwrap().unwrap();
        assert_eq!(third.signal, "deploy-complete");
        store.consume_signal("run", third.id).unwrap();
        assert!(store.oldest_pending_signal("run").unwrap().is_none());
    }

    #[test]
    fn scheduler_readiness_sealing_and_dynamic_additions_are_ordered() {
        let (_repo, _home, store, run) = store();
        let task = |name: &str, dependencies: &[&str], write_scope: &[&str]| TaskDefinition {
            name: name.into(),
            agent: "worker".into(),
            instructions: format!("instructions for {name}"),
            dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
            read_scope: vec![],
            write_scope: write_scope.iter().map(|value| (*value).into()).collect(),
            verification: vec![],
            verification_repair_limit: 0,
            review: None,
            boundary: crate::workflow::Boundary::Continue,
            context: vec![],
        };
        let definition = WorkflowDefinition {
            name: "ordered".into(),
            agents: [(
                "worker".into(),
                AgentDefinition {
                    profile: Some("implementation_worker".into()),
                    model: None,
                    reasoning_effort: None,
                    tool_profile: None,
                },
            )]
            .into_iter()
            .collect(),
            verification: vec![],
            boundary: crate::workflow::Boundary::Continue,
            context_packs: BTreeMap::new(),
            phases: vec![PhaseDefinition {
                name: "phase".into(),
                instructions: "phase instructions".into(),
                open: true,
                verification: vec![],
                boundary: crate::workflow::Boundary::Continue,
                review: None,
                tasks: vec![
                    task("first", &[], &["src/**"]),
                    task("second", &[], &["docs/**"]),
                    task("join", &["first", "second"], &["out/**"]),
                ],
            }],
        };
        store.initialize_workflow(&run, &definition).unwrap();

        let tasks = store.scheduled_tasks("run", "phase").unwrap();
        let task_name = |task: &ScheduledTask| store.scheduler_task(&task.task_id).unwrap().name;
        let declaration_order = tasks.iter().map(task_name).collect::<Vec<_>>();
        assert_eq!(declaration_order, ["first", "second", "join"]);

        let ready = store.ready_tasks("run", "phase").unwrap();
        assert_eq!(
            ready.iter().map(task_name).collect::<Vec<_>>(),
            ["first", "second"]
        );
        store.mark_task_running(&tasks[0].task_id).unwrap();
        let blocked_by_scope = store.ready_tasks("run", "phase").unwrap();
        assert_eq!(
            blocked_by_scope.iter().map(task_name).collect::<Vec<_>>(),
            ["second"]
        );

        store
            .queue_task(
                "run",
                "phase",
                "run:phase:late",
                &task("late", &["first"], &["late/**"]),
            )
            .unwrap();
        assert_eq!(
            store
                .scheduler_task("run:phase:late")
                .unwrap()
                .declaration_order,
            3
        );
        store.seal_phase("run", "phase").unwrap();
        assert!(
            store
                .queue_task(
                    "run",
                    "phase",
                    "run:phase:rejected",
                    &task("rejected", &[], &[])
                )
                .is_err()
        );
        assert!(store.scheduler_task("run:phase:rejected").is_err());
        assert!(store.phase_metadata("run", "phase").unwrap().sealed);
    }

    fn context_store() -> (tempfile::TempDir, FlowdexStore, RunInfo) {
        let (repo, _home, store, run) = store();
        let definition = WorkflowDefinition {
            name: "context".into(),
            agents: [(
                "worker".into(),
                AgentDefinition {
                    profile: Some("implementation_worker".into()),
                    model: None,
                    reasoning_effort: None,
                    tool_profile: None,
                },
            )]
            .into_iter()
            .collect(),
            verification: vec![],
            boundary: crate::workflow::Boundary::Continue,
            context_packs: BTreeMap::new(),
            phases: vec![PhaseDefinition {
                name: "phase".into(),
                instructions: "phase".into(),
                tasks: vec![],
                open: true,
                verification: vec![],
                boundary: crate::workflow::Boundary::Continue,
                review: None,
            }],
        };
        store.initialize_workflow(&run, &definition).unwrap();
        store
            .declare_context_packs(
                "run",
                &[(
                    "pack".into(),
                    ContextPackDeclaration {
                        agent: "worker".into(),
                        instructions: "collect".into(),
                    },
                )],
            )
            .unwrap();
        (repo, store, run)
    }

    #[test]
    fn context_publish_supersedes_and_resolves_fresh_or_stale() {
        let (repo, store, _run) = context_store();
        fs::write(repo.path().join("source.txt"), "one\ntwo\n").unwrap();
        let publication = ContextPublication {
            pack: "pack".into(),
            key: "source".into(),
            path: PathBuf::from("source.txt"),
            line_start: 1,
            line_end: 1,
            summary: Some("first".into()),
        };
        let publisher = ContextPublisher {
            thread_id: Some("thread".into()),
            agent_id: Some("worker".into()),
        };
        let first = store
            .publish_context_fragment("run", repo.path(), repo.path(), &publisher, &publication)
            .unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(
            store
                .resolve_context_pack("run", "pack", repo.path())
                .unwrap()
                .status,
            ContextPackStatus::Fresh
        );
        fs::write(repo.path().join("source.txt"), "changed\ntwo\n").unwrap();
        let stale = store
            .resolve_context_pack("run", "pack", repo.path())
            .unwrap();
        assert_eq!(stale.status, ContextPackStatus::Stale);
        assert_eq!(stale.stale_sources[0].key, "source");
        let second = store
            .publish_context_fragment(
                "run",
                repo.path(),
                repo.path(),
                &publisher,
                &ContextPublication {
                    summary: Some("second".into()),
                    ..publication
                },
            )
            .unwrap();
        assert_eq!(second.version, 2);
        let fresh = store
            .resolve_context_pack("run", "pack", repo.path())
            .unwrap();
        assert_eq!(fresh.status, ContextPackStatus::Fresh);
        assert_eq!(fresh.fragments[0].version, 2);
        let superseded: Option<i64> = store.runtime.block_on(sqlx::query_scalar(
            "SELECT superseded_version FROM context_fragments WHERE run_id='run' AND pack='pack' AND fragment_key='source' AND version=2"
        ).fetch_one(&store.pool)).unwrap();
        assert_eq!(superseded, Some(1));
    }

    #[test]
    fn context_publication_rejects_parent_escape() {
        let (repo, store, _run) = context_store();
        let outside = repo.path().parent().unwrap().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        let result = store.publish_context_fragment(
            "run",
            repo.path(),
            repo.path(),
            &ContextPublisher::default(),
            &ContextPublication {
                pack: "pack".into(),
                key: "escape".into(),
                path: PathBuf::from("../outside.txt"),
                line_start: 1,
                line_end: 1,
                summary: None,
            },
        );
        assert!(matches!(
            result,
            Err(FlowdexStoreError::Context(ContextError::InvalidPath(_)))
        ));
    }

    #[test]
    fn context_publication_reads_distinct_execution_worktree() {
        let (repo, store, _run) = context_store();
        let execution = tempdir().unwrap();
        fs::write(repo.path().join("source.txt"), "trusted\n").unwrap();
        fs::write(execution.path().join("source.txt"), "execution\n").unwrap();
        let fragment = store
            .publish_context_fragment(
                "run",
                execution.path(),
                repo.path(),
                &ContextPublisher::default(),
                &ContextPublication {
                    pack: "pack".into(),
                    key: "source".into(),
                    path: PathBuf::from("source.txt"),
                    line_start: 1,
                    line_end: 1,
                    summary: None,
                },
            )
            .unwrap();
        assert_eq!(fragment.content, "execution\n");
    }

    #[test]
    fn context_publication_rejects_final_component_link() {
        let (repo, store, _run) = context_store();
        let target = repo.path().join("target.txt");
        let link = repo.path().join("link.txt");
        fs::write(&target, "secret\n").unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, &link);
        if linked.is_err() {
            return;
        }
        let result = store.publish_context_fragment(
            "run",
            repo.path(),
            repo.path(),
            &ContextPublisher::default(),
            &ContextPublication {
                pack: "pack".into(),
                key: "link".into(),
                path: PathBuf::from("link.txt"),
                line_start: 1,
                line_end: 1,
                summary: None,
            },
        );
        assert!(matches!(
            result,
            Err(FlowdexStoreError::Context(ContextError::Reparse(_)))
        ));
    }
}
