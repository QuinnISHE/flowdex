use crate::workflow::{
    TaskDefinition, WorkflowDefinition, WorkflowValidationError, write_scope_conflicts,
};
use sha2::Digest;
use sha2::Sha256;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePoolOptions;
use std::ffi::OsStr;
use std::fs;
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
}

#[derive(Clone, Debug)]
pub struct RunInfo {
    pub run_id: String,
    pub parent_thread_id: String,
    pub workflow_path: String,
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
    Completed,
    Failed,
}
impl RunState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseState {
    Pending,
    Running,
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

pub struct FlowdexStore {
    pool: SqlitePool,
    runtime: tokio::runtime::Runtime,
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
            runtime,
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
        self.runtime.block_on(sqlx::query("INSERT INTO runs(run_id,parent_thread_id,workflow_path,repository_identity,integration_worktree,created_at) VALUES (?,?,?,?,?,?) ON CONFLICT(run_id) DO UPDATE SET parent_thread_id=excluded.parent_thread_id,workflow_path=excluded.workflow_path,integration_worktree=excluded.integration_worktree")
            .bind(&info.run_id).bind(&info.parent_thread_id).bind(&info.workflow_path).bind(&info.repository_identity).bind(info.integration_worktree.to_string_lossy().as_ref()).bind(now_unix()).execute(&self.pool))?;
        Ok(())
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
            sqlx::query("UPDATE runs SET name=?, state=? WHERE run_id=?")
                .bind(&definition.name).bind(RunState::Queued.as_str()).bind(&info.run_id)
                .execute(&mut *tx).await?;
            for (agent_name, agent) in &definition.agents {
                sqlx::query("INSERT INTO workflow_agents(run_id,name,profile,model,reasoning_effort) VALUES (?,?,?,?,?)")
                    .bind(&info.run_id).bind(agent_name).bind(agent.profile.as_deref()).bind(agent.model.as_deref()).bind(agent.reasoning_effort.as_deref()).execute(&mut *tx).await?;
            }
            for (index, phase) in definition.phases.iter().enumerate() {
                sqlx::query("INSERT INTO workflow_phases(run_id,name,declaration_order,instructions,open,sealed,verification,state) VALUES (?,?,?,?,?,?,?,?)")
                    .bind(&info.run_id).bind(&phase.name).bind(index as i64).bind(&phase.instructions)
                    .bind(phase.open as i64).bind((!phase.open) as i64).bind(encode(&phase.verification)).bind(if phase.open { PhaseState::Pending.as_str() } else { PhaseState::Sealed.as_str() })
                    .execute(&mut *tx).await?;
                for (task_index, task) in phase.tasks.iter().enumerate() {
                    let task_id = format!("{}:{}:{}", info.run_id, phase.name, task.name);
                    insert_schedule_row(&mut tx, &task_id, &info.run_id, &phase.name, task_index as i64, task, SchedulerTaskState::Queued).await?;
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
        self.runtime.block_on(async {
            let mut tx = self.pool.begin().await?;
            let phase = sqlx::query("SELECT open,sealed FROM workflow_phases WHERE run_id=? AND name=?")
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
            insert_schedule_row(&mut tx, task_id, run_id, phase_name, order, task, SchedulerTaskState::Queued).await?;
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
        self.runtime.block_on(
            sqlx::query("UPDATE runs SET state=? WHERE run_id=?")
                .bind(state.as_str())
                .bind(run_id)
                .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn set_phase_state(
        &self,
        run_id: &str,
        phase_name: &str,
        state: PhaseState,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(
            sqlx::query("UPDATE workflow_phases SET state=? WHERE run_id=? AND name=?")
                .bind(state.as_str())
                .bind(run_id)
                .bind(phase_name)
                .execute(&self.pool),
        )?;
        Ok(())
    }

    pub fn set_scheduler_task_state(
        &self,
        task_id: &str,
        state: SchedulerTaskState,
    ) -> Result<(), FlowdexStoreError> {
        self.runtime.block_on(
            sqlx::query("UPDATE workflow_tasks SET state=? WHERE task_id=?")
                .bind(state.as_str())
                .bind(task_id)
                .execute(&self.pool),
        )?;
        Ok(())
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
            let running_scopes: Vec<(i64, Vec<String>)> = rows.iter().filter(|row| row.get::<String, _>(5) == "running").map(|row| (row.get(2), decode(row.get(7)))).collect();
            let mut ready = Vec::with_capacity(rows.len());
            for row in rows {
                let candidate = ScheduledTask { task_id: row.get(0), phase: row.get(1), declaration_order: row.get(2), agent: row.get(3), dependencies: decode(row.get(4)), state: row.get(5) };
                let candidate_scope: Vec<String> = decode(row.get(7));
                let blocked_by_scope = running_scopes.iter().any(|(order, scope)| *order != candidate.declaration_order && *order < candidate.declaration_order && write_scope_conflicts(&candidate_scope, scope));
                if matches!(candidate.state.as_str(), "queued" | "ready") && candidate.dependencies.iter().all(|dependency| states.get(dependency).is_some_and(|state| state == "integrated")) && !blocked_by_scope { ready.push(candidate); }
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
        let task = self.task(task_id)?;
        if task.state == "integrated" {
            return Err(FlowdexStoreError::Integration(
                "task is already integrated".to_string(),
            ));
        }
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
        self.runtime.block_on(self.integrate_locked(task_id, task))
    }

    async fn integrate_locked(
        &self,
        task_id: &str,
        task: TaskRecord,
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
        let rows = sqlx::query("SELECT source_commit,agent_id,model,summary FROM task_commits WHERE task_id=? ORDER BY sequence").bind(task_id).fetch_all(&mut *tx).await?;
        let source: Vec<TaskCommit> = rows
            .into_iter()
            .map(|row| TaskCommit {
                source_commit: row.get(0),
                integrated_commit: None,
                agent_id: row.get(1),
                model: row.get(2),
                summary: row.get(3),
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
        for commit in &source {
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
        if let Err(error) = remove_task_worktree(
            &self.integration_worktree,
            &self.worktree_root,
            &task.worktree_path,
        ) {
            rollback_integration(&self.integration_worktree, &pre_head)?;
            return Err(error);
        }
        tx.commit().await?;
        Ok(IntegrationResult {
            task_id: task_id.to_string(),
            commits: result_commits,
        })
    }
}

async fn insert_schedule_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    run_id: &str,
    phase: &str,
    declaration_order: i64,
    task: &TaskDefinition,
    state: SchedulerTaskState,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO workflow_tasks(task_id,run_id,phase,name,declaration_order,agent,dependencies,read_scope,write_scope,verification,state) VALUES (?,?,?,?,?,?,?,?,?,?,?)")
        .bind(task_id).bind(run_id).bind(phase).bind(&task.name).bind(declaration_order).bind(&task.agent)
        .bind(encode(&task.dependencies)).bind(encode(&task.read_scope)).bind(encode(&task.write_scope)).bind(encode(&task.verification)).bind(state.as_str())
        .execute(&mut **tx).await?;
    Ok(())
}

async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let statements = "PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS repository(identity TEXT NOT NULL PRIMARY KEY);
      CREATE TABLE IF NOT EXISTS runs(run_id TEXT PRIMARY KEY,parent_thread_id TEXT NOT NULL,workflow_path TEXT NOT NULL,repository_identity TEXT NOT NULL,integration_worktree TEXT NOT NULL,created_at INTEGER NOT NULL,name TEXT NOT NULL DEFAULT '',state TEXT NOT NULL DEFAULT 'queued');
      CREATE TABLE IF NOT EXISTS tasks(task_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,name TEXT NOT NULL,instructions TEXT NOT NULL,read_scope TEXT NOT NULL,write_scope TEXT NOT NULL,verification TEXT NOT NULL,base_commit TEXT NOT NULL,worktree_path TEXT NOT NULL,state TEXT NOT NULL,last_verified_commit TEXT);
      CREATE TABLE IF NOT EXISTS workflow_phases(run_id TEXT NOT NULL,name TEXT NOT NULL,declaration_order INTEGER NOT NULL,instructions TEXT NOT NULL,open INTEGER NOT NULL,sealed INTEGER NOT NULL,verification TEXT NOT NULL,state TEXT NOT NULL,PRIMARY KEY(run_id,name));
      CREATE TABLE IF NOT EXISTS workflow_agents(run_id TEXT NOT NULL,name TEXT NOT NULL,profile TEXT,model TEXT,reasoning_effort TEXT,PRIMARY KEY(run_id,name));
      CREATE TABLE IF NOT EXISTS workflow_tasks(task_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,phase TEXT NOT NULL,name TEXT NOT NULL,declaration_order INTEGER NOT NULL,agent TEXT NOT NULL,dependencies TEXT NOT NULL,read_scope TEXT NOT NULL,write_scope TEXT NOT NULL,verification TEXT NOT NULL,state TEXT NOT NULL,UNIQUE(run_id,phase,name));
      CREATE TABLE IF NOT EXISTS task_operations(operation_id TEXT PRIMARY KEY,task_id TEXT NOT NULL,agent_id TEXT NOT NULL,model TEXT NOT NULL,start_commit TEXT NOT NULL,terminal_state TEXT,sequence INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS task_commits(task_id TEXT NOT NULL,source_commit TEXT NOT NULL,integrated_commit TEXT,operation_id TEXT NOT NULL,agent_id TEXT NOT NULL,model TEXT NOT NULL,sequence INTEGER NOT NULL,summary TEXT NOT NULL,PRIMARY KEY(task_id,source_commit));
      CREATE TABLE IF NOT EXISTS integration_lock(id INTEGER PRIMARY KEY CHECK(id=1),generation INTEGER NOT NULL);
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
    ] {
        let _ = sqlx::query(statement).execute(pool).await;
    }
    Ok(())
}

fn encode(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
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

fn bound_git_output(mut text: String) -> String {
    if text.len() > MAX_GIT_OUTPUT {
        let mut end = MAX_GIT_OUTPUT;
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
    use codex_utils_absolute_path::AbsolutePathBuf;
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
            repository_identity: root.to_string_lossy().to_string(),
            integration_worktree: repo.path().into(),
        };
        (repo, home, store, run)
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
}
