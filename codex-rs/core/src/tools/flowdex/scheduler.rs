use super::FlowdexResumeAgentHandler;
use super::task::{self, AgentSpec};
use super::verification::FlowdexVerifyHandler;
use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload, boxed_tool_output};
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::{
    FlowdexTaskIntegrateHandler, FlowdexTaskRunAgentHandler, FlowdexTaskVerifyHandler,
};
use crate::tools::registry::{CoreToolRuntime, ToolExecutor};
use codex_flowdex::context::{
    ContextPackDeclaration, ContextPackStatus, ContextStaleSource, ResolvedContextPack,
};
use codex_flowdex::store::{
    FlowdexStore, FlowdexStoreError, PendingBoundary, ReviewFinding, ReviewResolution, RunInfo,
    RunState, SchedulerTaskState, TaskDeclaration,
};
use codex_flowdex::workflow::ContextPackDefinition;
use codex_flowdex::{
    Boundary, ContextFragmentSeed, ContextPackLifetime, PhaseDefinition, ReviewDefinition,
    TaskDefinition, WorkflowDefinition,
};
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::items::{ReasoningItem, TurnItem};
use codex_protocol::openai_models::ReasoningEffort;
use codex_tools::shell_command_backend_for_features;
use codex_tools::{JsonSchema, ResponsesApiTool, ToolName, ToolSpec};
use futures::future::join_all;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

const START: &str = "flowdex_start_run";
const QUEUE: &str = "flowdex_queue_task";
const SEAL: &str = "flowdex_seal_phase";
const WAIT: &str = "flowdex_wait_run";
const DIRECT_QUEUE: &str = "queue_flowdex_task";
const DIRECT_SEAL: &str = "seal_flowdex_phase";
const DIRECT_PAUSE: &str = "pause_flowdex_workflow";
const DIRECT_RESUME: &str = "resume_flowdex_workflow";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    definition: RawWorkflow,
    workflow_path: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueArgs {
    run_id: String,
    phase: String,
    task: RawTask,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealArgs {
    run_id: String,
    phase: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    run_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkflow {
    name: String,
    agents: std::collections::BTreeMap<String, RawAgent>,
    phases: Vec<RawPhase>,
    #[serde(default)]
    verification: Vec<String>,
    #[serde(default)]
    cleanup: Vec<String>,
    #[serde(default)]
    context_packs: std::collections::BTreeMap<String, RawContextPack>,
    #[serde(default)]
    boundary: Boundary,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContextPack {
    agent: String,
    instructions: String,
    #[serde(default)]
    lifetime: ContextPackLifetime,
    #[serde(default)]
    fragments: Vec<RawContextFragment>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContextFragment {
    key: String,
    path: std::path::PathBuf,
    line_start: u32,
    line_end: u32,
    #[serde(default)]
    summary: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    tool_profile: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPhase {
    name: String,
    instructions: String,
    tasks: Vec<RawTask>,
    #[serde(default)]
    open: bool,
    #[serde(default)]
    verification: Vec<String>,
    #[serde(default)]
    boundary: Boundary,
    #[serde(default)]
    review: Option<ReviewDefinition>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    name: String,
    agent: String,
    instructions: String,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    read_scope: Vec<String>,
    #[serde(default)]
    write_scope: Vec<String>,
    #[serde(default)]
    verification: Vec<String>,
    #[serde(default)]
    verification_repair_limit: usize,
    #[serde(default)]
    review: Option<ReviewDefinition>,
    #[serde(default)]
    boundary: Boundary,
    #[serde(default)]
    context: Vec<String>,
}
impl From<RawTask> for TaskDefinition {
    fn from(t: RawTask) -> Self {
        Self {
            name: t.name,
            agent: t.agent,
            instructions: t.instructions,
            dependencies: t.dependencies,
            read_scope: t.read_scope,
            write_scope: t.write_scope,
            verification: t.verification,
            verification_repair_limit: t.verification_repair_limit,
            review: t.review,
            boundary: t.boundary,
            context: t.context,
        }
    }
}
impl From<RawWorkflow> for WorkflowDefinition {
    fn from(w: RawWorkflow) -> Self {
        Self {
            name: w.name,
            agents: w
                .agents
                .into_iter()
                .map(|(n, a)| {
                    (
                        n,
                        codex_flowdex::AgentDefinition {
                            profile: a.profile,
                            model: a.model,
                            reasoning_effort: a.reasoning_effort,
                            tool_profile: a.tool_profile,
                        },
                    )
                })
                .collect(),
            phases: w
                .phases
                .into_iter()
                .map(|p| PhaseDefinition {
                    name: p.name,
                    instructions: p.instructions,
                    tasks: p.tasks.into_iter().map(Into::into).collect(),
                    open: p.open,
                    verification: p.verification,
                    boundary: p.boundary,
                    review: p.review,
                })
                .collect(),
            verification: w.verification,
            cleanup: w.cleanup,
            boundary: w.boundary,
            context_packs: w
                .context_packs
                .into_iter()
                .map(|(name, pack)| {
                    (
                        name,
                        ContextPackDefinition {
                            agent: pack.agent,
                            instructions: pack.instructions,
                            lifetime: pack.lifetime,
                            fragments: pack
                                .fragments
                                .into_iter()
                                .map(|fragment| ContextFragmentSeed {
                                    key: fragment.key,
                                    path: fragment.path,
                                    line_start: fragment.line_start,
                                    line_end: fragment.line_end,
                                    summary: fragment.summary,
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RunStatus {
    Running,
    Paused { interrupted: bool },
    Completed,
    Failed(String),
}

struct RunController {
    id: String,
    definition: Mutex<WorkflowDefinition>,
    invocation: ToolInvocation,
    store: Arc<FlowdexStore>,
    info: RunInfo,
    status: watch::Sender<RunStatus>,
    status_rx: watch::Receiver<RunStatus>,
    activity: watch::Sender<u64>,
    activity_rx: watch::Receiver<u64>,
    pause: watch::Sender<bool>,
    pause_rx: watch::Receiver<bool>,
    detached_resume: bool,
    context_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

static RUNS: OnceLock<Mutex<HashMap<String, Arc<RunController>>>> = OnceLock::new();
static RESUMING_RUNS: OnceLock<std::sync::Mutex<HashSet<String>>> = OnceLock::new();
fn runs() -> &'static Mutex<HashMap<String, Arc<RunController>>> {
    RUNS.get_or_init(Default::default)
}

struct ResumeClaim(String);

impl ResumeClaim {
    fn acquire(run_id: &str) -> Result<Self, FunctionCallError> {
        let mut claims = RESUMING_RUNS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| {
                FunctionCallError::RespondToModel("Flowdex resume state unavailable".into())
            })?;
        if !claims.insert(run_id.to_string()) {
            return Err(FunctionCallError::RespondToModel(
                "Flowdex run is already being resumed".into(),
            ));
        }
        Ok(Self(run_id.to_string()))
    }
}

impl Drop for ResumeClaim {
    fn drop(&mut self) {
        if let Ok(mut claims) = RESUMING_RUNS.get_or_init(Default::default).lock() {
            claims.remove(&self.0);
        }
    }
}

pub(crate) struct FlowdexStartRunHandler;
pub(crate) struct FlowdexQueueTaskHandler;
pub(crate) struct FlowdexSealPhaseHandler;
pub(crate) struct FlowdexWaitRunHandler;
pub(crate) struct QueueFlowdexTaskHandler;
pub(crate) struct SealFlowdexPhaseHandler;
pub(crate) struct PauseFlowdexWorkflowHandler;
pub(crate) struct ResumeFlowdexWorkflowHandler;

macro_rules! handler {
    ($ty:ty, $name:expr, $spec:ident, $call:ident) => {
        impl ToolExecutor<ToolInvocation> for $ty {
            fn tool_name(&self) -> ToolName {
                ToolName::plain($name)
            }
            fn spec(&self) -> ToolSpec {
                $spec()
            }
            fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
                Box::pin(async move { $call(invocation).await.map(boxed_tool_output) })
            }
        }
        impl CoreToolRuntime for $ty {}
    };
}
handler!(FlowdexStartRunHandler, START, start_spec, start_call);
handler!(FlowdexQueueTaskHandler, QUEUE, queue_spec, queue_call);
handler!(FlowdexSealPhaseHandler, SEAL, seal_spec, seal_call);
handler!(FlowdexWaitRunHandler, WAIT, wait_spec, wait_call);

impl ToolExecutor<ToolInvocation> for QueueFlowdexTaskHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(DIRECT_QUEUE)
    }
    fn spec(&self) -> ToolSpec {
        queue_spec_named(DIRECT_QUEUE)
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { queue_call(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for QueueFlowdexTaskHandler {}

impl ToolExecutor<ToolInvocation> for SealFlowdexPhaseHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(DIRECT_SEAL)
    }
    fn spec(&self) -> ToolSpec {
        seal_spec_named(DIRECT_SEAL)
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { seal_call(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for SealFlowdexPhaseHandler {}

handler!(
    PauseFlowdexWorkflowHandler,
    DIRECT_PAUSE,
    pause_spec,
    pause_call
);
handler!(
    ResumeFlowdexWorkflowHandler,
    DIRECT_RESUME,
    resume_spec,
    resume_call
);

fn spec(
    name: &str,
    description: &str,
    properties: Vec<(&str, JsonSchema)>,
    required: Vec<&str>,
) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.into(),
        description: description.into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            Some(required.into_iter().map(str::to_string).collect()),
            Some(false.into()),
        ),
        output_schema: Some(serde_json::json!({"type": "object"})),
    })
}
fn start_spec() -> ToolSpec {
    spec(
        START,
        "Start a durable Flowdex workflow run.",
        vec![
            ("definition", JsonSchema::default()),
            ("workflow_path", JsonSchema::string(None)),
        ],
        vec!["definition", "workflow_path"],
    )
}
fn queue_spec() -> ToolSpec {
    queue_spec_named(QUEUE)
}
fn queue_spec_named(name: &str) -> ToolSpec {
    spec(
        name,
        "Queue a task in an open Flowdex phase.",
        vec![
            ("run_id", JsonSchema::string(None)),
            ("phase", JsonSchema::string(None)),
            ("task", JsonSchema::default()),
        ],
        vec!["run_id", "phase", "task"],
    )
}
fn seal_spec() -> ToolSpec {
    seal_spec_named(SEAL)
}
fn seal_spec_named(name: &str) -> ToolSpec {
    spec(
        name,
        "Seal an open Flowdex phase.",
        vec![
            ("run_id", JsonSchema::string(None)),
            ("phase", JsonSchema::string(None)),
        ],
        vec!["run_id", "phase"],
    )
}
fn wait_spec() -> ToolSpec {
    spec(
        WAIT,
        "Wait for a Flowdex run to complete.",
        vec![("run_id", JsonSchema::string(None))],
        vec!["run_id"],
    )
}
fn pause_spec() -> ToolSpec {
    spec(
        DIRECT_PAUSE,
        "Pause a Flowdex workflow at its next stable scheduler checkpoint.",
        vec![("run_id", JsonSchema::string(None))],
        vec!["run_id"],
    )
}
fn resume_spec() -> ToolSpec {
    spec(
        DIRECT_RESUME,
        "Resume a paused, interrupted, or failed Flowdex workflow from durable state.",
        vec![("run_id", JsonSchema::string(None))],
        vec!["run_id"],
    )
}

async fn start_call(invocation: ToolInvocation) -> Result<task::JsonOutput, FunctionCallError> {
    let ToolInvocation {
        session,
        turn: _,
        payload: ToolPayload::Function { arguments },
        ..
    } = invocation.clone()
    else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex_start_run expects JSON arguments".into(),
        ));
    };
    let args: StartArgs = parse_arguments(&arguments)?;
    let definition: WorkflowDefinition = args.definition.into();
    definition
        .validate()
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let run_id = task::runtime_id(&invocation)?;
    let (parent_run_id, workflow_identity) = super::workflow_run_identity(&run_id);
    let (store, cwd, identity) = task::task_store(&invocation).await?;
    let info = RunInfo {
        run_id: run_id.clone(),
        parent_thread_id: session.thread_id.to_string(),
        workflow_path: args.workflow_path,
        parent_run_id,
        workflow_identity,
        repository_identity: identity,
        integration_worktree: cwd,
    };
    let store = Arc::new(store);
    let store_for_init = Arc::clone(&store);
    let info_for_init = info.clone();
    let def_for_init = definition.clone();
    tokio::task::spawn_blocking(move || {
        store_for_init.initialize_workflow(&info_for_init, &def_for_init)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let declarations = definition
        .context_packs
        .iter()
        .map(|(name, pack)| {
            (
                name.clone(),
                ContextPackDeclaration {
                    agent: pack.agent.clone(),
                    instructions: pack.instructions.clone(),
                    lifetime: pack.lifetime,
                    fragments: pack.fragments.clone(),
                },
            )
        })
        .collect::<Vec<_>>();
    if !declarations.is_empty() {
        let store_for_context = Arc::clone(&store);
        let run_id_for_context = run_id.clone();
        let declarations_for_declare = declarations.clone();
        tokio::task::spawn_blocking(move || {
            store_for_context.declare_context_packs(&run_id_for_context, &declarations_for_declare)
        })
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
        let store_for_hydration = Arc::clone(&store);
        let run_id_for_hydration = run_id.clone();
        let integration_worktree = info.integration_worktree.clone();
        let declarations_for_hydration = declarations.clone();
        tokio::task::spawn_blocking(move || {
            store_for_hydration.hydrate_context_packs(
                &run_id_for_hydration,
                &integration_worktree,
                &declarations_for_hydration,
            )
        })
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    }
    let (status, status_rx) = watch::channel(RunStatus::Running);
    let (activity, activity_rx) = watch::channel(0u64);
    let (pause, pause_rx) = watch::channel(false);
    let controller = Arc::new(RunController {
        id: run_id.clone(),
        definition: Mutex::new(definition),
        invocation: invocation.clone(),
        store,
        info,
        status,
        status_rx,
        activity,
        activity_rx,
        pause,
        pause_rx,
        detached_resume: false,
        context_gates: Mutex::new(HashMap::new()),
    });
    runs()
        .lock()
        .await
        .insert(run_id.clone(), Arc::clone(&controller));
    super::register_flowdex_boundary_run(run_id.clone(), Arc::clone(&controller.store));
    // The scheduler owns the live run independently of the tool invocation turn.
    tokio::spawn(run_scheduler(controller));
    Ok(task::JsonOutput(serde_json::json!({"runId": run_id})))
}

async fn queue_call(invocation: ToolInvocation) -> Result<task::JsonOutput, FunctionCallError> {
    let ToolPayload::Function { arguments } = invocation.payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex_queue_task expects JSON arguments".into(),
        ));
    };
    let args: QueueArgs = parse_arguments(&arguments)?;
    let controller = runs()
        .lock()
        .await
        .get(&args.run_id)
        .cloned()
        .ok_or_else(|| FunctionCallError::RespondToModel("Flowdex run not found".into()))?;
    let task: TaskDefinition = args.task.into();
    {
        let definition = controller.definition.lock().await;
        definition
            .validate_dynamic_task(&args.phase, &task)
            .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    }
    let task_id = Uuid::new_v4().to_string();
    let store = Arc::clone(&controller.store);
    let run_id = args.run_id.clone();
    let phase = args.phase.clone();
    let task_id_for_store = task_id.clone();
    let queued_task = task.clone();
    tokio::task::spawn_blocking(move || {
        store.queue_task(&run_id, &phase, &task_id_for_store, &queued_task)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let phase_instructions = {
        let mut definition = controller.definition.lock().await;
        let phase = definition
            .phases
            .iter_mut()
            .find(|p| p.name == args.phase)
            .ok_or_else(|| FunctionCallError::RespondToModel("Flowdex phase not found".into()))?;
        let instructions = phase.instructions.clone();
        phase.tasks.push(task.clone());
        instructions
    };
    let declaration = TaskDeclaration {
        id: task_id.clone(),
        name: task.name.clone(),
        instructions: format!("{}\n\n{}", phase_instructions, task.instructions),
        read_scope: task.read_scope.clone(),
        write_scope: task.write_scope.clone(),
        verification: task.verification.clone(),
    };
    let store = Arc::clone(&controller.store);
    let info = controller.info.clone();
    let declaration_for_store = declaration.clone();
    tokio::task::spawn_blocking(move || store.create_task(&info, &declaration_for_store))
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let next_activity = controller.activity_rx.borrow().wrapping_add(1);
    let _ = controller.activity.send(next_activity);
    Ok(task::JsonOutput(serde_json::json!({"taskId": task_id})))
}

async fn seal_call(invocation: ToolInvocation) -> Result<task::JsonOutput, FunctionCallError> {
    let ToolPayload::Function { arguments } = invocation.payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex_seal_phase expects JSON arguments".into(),
        ));
    };
    let args: SealArgs = parse_arguments(&arguments)?;
    let controller = runs()
        .lock()
        .await
        .get(&args.run_id)
        .cloned()
        .ok_or_else(|| FunctionCallError::RespondToModel("Flowdex run not found".into()))?;
    let store = Arc::clone(&controller.store);
    let run_id = args.run_id.clone();
    let phase = args.phase.clone();
    tokio::task::spawn_blocking(move || store.seal_phase(&run_id, &phase))
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let next_activity = controller.activity_rx.borrow().wrapping_add(1);
    let _ = controller.activity.send(next_activity);
    Ok(task::JsonOutput(
        serde_json::json!({"runId": args.run_id, "phase": args.phase}),
    ))
}

async fn pause_call(invocation: ToolInvocation) -> Result<task::JsonOutput, FunctionCallError> {
    let ToolPayload::Function { arguments } = invocation.payload else {
        return Err(FunctionCallError::RespondToModel(
            "pause_flowdex_workflow expects JSON arguments".into(),
        ));
    };
    let args: WaitArgs = parse_arguments(&arguments)?;
    let controller = runs()
        .lock()
        .await
        .get(&args.run_id)
        .cloned()
        .ok_or_else(|| FunctionCallError::RespondToModel("Flowdex run is not active".into()))?;
    controller.pause.send_replace(true);
    let next_activity = controller.activity_rx.borrow().wrapping_add(1);
    let _ = controller.activity.send(next_activity);
    let mut status = controller.status_rx.clone();
    loop {
        let current = status.borrow_and_update().clone();
        match current {
            RunStatus::Paused { .. } => {
                return Ok(task::JsonOutput(
                    serde_json::json!({"runId": args.run_id, "status": "paused"}),
                ));
            }
            RunStatus::Failed(error) => {
                return Err(FunctionCallError::RespondToModel(error));
            }
            RunStatus::Completed => {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex run already completed".into(),
                ));
            }
            RunStatus::Running => {
                status
                    .changed()
                    .await
                    .map_err(|_| FunctionCallError::RespondToModel("Flowdex run stopped".into()))?;
            }
        }
    }
}

async fn resume_call(invocation: ToolInvocation) -> Result<task::JsonOutput, FunctionCallError> {
    let ToolPayload::Function { arguments } = invocation.payload.clone() else {
        return Err(FunctionCallError::RespondToModel(
            "resume_flowdex_workflow expects JSON arguments".into(),
        ));
    };
    let args: WaitArgs = parse_arguments(&arguments)?;
    let _claim = ResumeClaim::acquire(&args.run_id)?;
    if let Some(controller) = runs().lock().await.get(&args.run_id).cloned() {
        let current = controller.status_rx.borrow().clone();
        match current {
            RunStatus::Paused { interrupted: false } => {
                let store = Arc::clone(&controller.store);
                let run_id = args.run_id.clone();
                tokio::task::spawn_blocking(move || store.prepare_run_resume(&run_id))
                    .await
                    .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
                    .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
                controller.pause.send_replace(false);
                let _ = controller.status.send(RunStatus::Running);
                tokio::spawn(run_scheduler(controller));
                return Ok(task::JsonOutput(
                    serde_json::json!({"runId": args.run_id, "status": "resumed"}),
                ));
            }
            RunStatus::Paused { interrupted: true } => {
                remove_run_controller(&args.run_id).await;
            }
            RunStatus::Running => {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex run is already active".into(),
                ));
            }
            RunStatus::Completed => {
                return Err(FunctionCallError::RespondToModel(
                    "completed Flowdex runs cannot be resumed".into(),
                ));
            }
            RunStatus::Failed(_) => {
                remove_run_controller(&args.run_id).await;
            }
        }
    }

    let (store, cwd, repository_identity) =
        task::open_store(&invocation.session, &invocation.turn).await?;
    let store = Arc::new(store);
    let run_id = args.run_id.clone();
    let store_for_load = Arc::clone(&store);
    let (info, definition) = tokio::task::spawn_blocking(move || {
        let info = store_for_load.run_info(&run_id)?;
        let definition = store_for_load.workflow_definition(&run_id)?;
        Ok::<_, FlowdexStoreError>((info, definition))
    })
    .await
    .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
    .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    if info.repository_identity != repository_identity {
        return Err(FunctionCallError::RespondToModel(
            "Flowdex run belongs to another repository".into(),
        ));
    }
    let persisted_worktree = std::fs::canonicalize(&info.integration_worktree)
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    let current_worktree = std::fs::canonicalize(cwd)
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    if persisted_worktree != current_worktree {
        return Err(FunctionCallError::RespondToModel(
            "Flowdex run must be resumed from its original integration worktree".into(),
        ));
    }
    let store_for_resume = Arc::clone(&store);
    let run_id = args.run_id.clone();
    tokio::task::spawn_blocking(move || store_for_resume.prepare_run_resume(&run_id))
        .await
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    let (status, status_rx) = watch::channel(RunStatus::Running);
    let (activity, activity_rx) = watch::channel(0u64);
    let (pause, pause_rx) = watch::channel(false);
    let controller = Arc::new(RunController {
        id: args.run_id.clone(),
        definition: Mutex::new(definition),
        invocation,
        store,
        info,
        status,
        status_rx,
        activity,
        activity_rx,
        pause,
        pause_rx,
        detached_resume: true,
        context_gates: Mutex::new(HashMap::new()),
    });
    runs()
        .lock()
        .await
        .insert(args.run_id.clone(), Arc::clone(&controller));
    super::register_flowdex_boundary_run(args.run_id.clone(), Arc::clone(&controller.store));
    tokio::spawn(run_scheduler(controller));
    Ok(task::JsonOutput(
        serde_json::json!({"runId": args.run_id, "status": "resumed"}),
    ))
}

async fn wait_call(invocation: ToolInvocation) -> Result<task::JsonOutput, FunctionCallError> {
    let ToolPayload::Function { arguments } = invocation.payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex_wait_run expects JSON arguments".into(),
        ));
    };
    let args: WaitArgs = parse_arguments(&arguments)?;
    let controller = runs()
        .lock()
        .await
        .get(&args.run_id)
        .cloned()
        .ok_or_else(|| FunctionCallError::RespondToModel("Flowdex run not found".into()))?;
    let mut rx = controller.status_rx.clone();
    loop {
        let status = rx.borrow_and_update().clone();
        match status {
            RunStatus::Running | RunStatus::Paused { .. } => {
                rx.changed()
                    .await
                    .map_err(|_| FunctionCallError::RespondToModel("Flowdex run stopped".into()))?;
            }
            RunStatus::Completed => {
                remove_run_controller(&args.run_id).await;
                return Ok(task::JsonOutput(
                    serde_json::json!({"runId": args.run_id, "status": "completed"}),
                ));
            }
            RunStatus::Failed(error) => {
                remove_run_controller(&args.run_id).await;
                return Ok(task::JsonOutput(serde_json::json!({
                    "runId": args.run_id,
                    "status": "failed",
                    "error": error,
                })));
            }
        }
    }
}

async fn remove_run_controller(run_id: &str) {
    if let Some(controller) = runs().lock().await.remove(run_id) {
        // Terminal delivery must not wait for potentially blocking store disposal.
        let _ = tokio::task::spawn_blocking(move || drop(controller));
    }
}

pub(super) enum SchedulerEvent {
    Paused,
    Completed,
    Failed(String),
}

pub(super) async fn is_detached_resume(run_id: &str) -> bool {
    runs()
        .lock()
        .await
        .get(run_id)
        .is_some_and(|controller| controller.detached_resume)
}

pub(super) async fn wait_for_run_event(run_id: String) -> SchedulerEvent {
    let Some(controller) = runs().lock().await.get(&run_id).cloned() else {
        return std::future::pending().await;
    };
    let mut status = controller.status_rx.clone();
    loop {
        let current = status.borrow_and_update().clone();
        match current {
            RunStatus::Running => {
                if status.changed().await.is_err() {
                    return std::future::pending().await;
                }
            }
            RunStatus::Paused { .. } => return SchedulerEvent::Paused,
            RunStatus::Completed if controller.detached_resume => {
                remove_run_controller(&run_id).await;
                return SchedulerEvent::Completed;
            }
            RunStatus::Completed => return std::future::pending().await,
            RunStatus::Failed(error) => {
                remove_run_controller(&run_id).await;
                return SchedulerEvent::Failed(error);
            }
        }
    }
}

enum SchedulerOutcome {
    Completed,
    Paused { interrupted: bool },
}

async fn run_scheduler(controller: Arc<RunController>) {
    let result = tokio::select! {
        _ = controller.invocation.cancellation_token.cancelled() => {
            Ok(SchedulerOutcome::Paused { interrupted: true })
        }
        result = run_scheduler_inner(&controller) => result,
    };
    match result {
        Err(error) => {
            let store = Arc::clone(&controller.store);
            let run_id = controller.id.clone();
            let persisted_error = error.clone();
            let _ = tokio::task::spawn_blocking(move || {
                store.mark_run_failed(&run_id, &persisted_error)
            })
            .await;
            super::mark_flowdex_boundary_terminal(&controller.id);
            let _ = controller.status.send(RunStatus::Failed(error));
        }
        Ok(SchedulerOutcome::Paused { interrupted }) => {
            let store = Arc::clone(&controller.store);
            let run_id = controller.id.clone();
            let _ = tokio::task::spawn_blocking(move || store.mark_run_paused(&run_id)).await;
            progress(&controller.invocation, "Paused workflow".to_string()).await;
            let _ = controller.status.send(RunStatus::Paused { interrupted });
        }
        Ok(SchedulerOutcome::Completed) => {
            super::mark_flowdex_boundary_terminal(&controller.id);
            let _ = controller.status.send(RunStatus::Completed);
        }
    }
}

async fn run_scheduler_inner(controller: &Arc<RunController>) -> Result<SchedulerOutcome, String> {
    let definition = controller.definition.lock().await.clone();
    let total = definition.phases.len();
    let pending_boundary = {
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        tokio::task::spawn_blocking(move || store.pending_boundary(&run_id))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
    };
    if let Some(boundary) = pending_boundary {
        super::publish_flowdex_boundary(Arc::clone(&controller.store), boundary.clone()).await?;
        super::wait_flowdex_boundary_continuation(
            &boundary.run_id,
            &boundary.scope_kind,
            &boundary.scope_id,
        )
        .await;
        if boundary.scope_kind == "run" {
            finish_successful_run(controller, &definition).await?;
            return Ok(SchedulerOutcome::Completed);
        }
    }
    progress(
        &controller.invocation,
        format!("Running workflow: {}", definition.name),
    )
    .await;
    let set_run = (
        Arc::clone(&controller.store),
        controller.info.run_id.clone(),
    );
    tokio::task::spawn_blocking(move || set_run.0.set_run_state(&set_run.1, RunState::Running))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    for (index, phase) in definition.phases.iter().enumerate() {
        if pause_requested(controller) {
            return Ok(SchedulerOutcome::Paused { interrupted: false });
        }
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        let phase_name = phase.name.clone();
        let state = tokio::task::spawn_blocking(move || {
            store
                .phase_metadata(&run_id, &phase_name)
                .map(|phase| phase.state)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        if state == "completed" {
            if phase.review.is_some() {
                let store = Arc::clone(&controller.store);
                let run_id = controller.id.clone();
                let phase_name = phase.name.clone();
                let tasks = tokio::task::spawn_blocking(move || {
                    store.scheduled_tasks(&run_id, &phase_name)
                })
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
                for task in tasks {
                    close_task_agents(controller, &task.task_id).await?;
                    let store = Arc::clone(&controller.store);
                    let task_id = task.task_id;
                    tokio::task::spawn_blocking(move || {
                        store.recover_integrated_task_cleanup(&task_id)
                    })
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
                }
            }
            continue;
        }
        progress(
            &controller.invocation,
            format!("Running phase {}/{}: {}", index + 1, total, phase.name),
        )
        .await;
        if matches!(
            run_phase(controller, index, total).await?,
            SchedulerOutcome::Paused { .. }
        ) {
            return Ok(SchedulerOutcome::Paused { interrupted: false });
        }
    }
    if !definition.verification.is_empty() {
        if pause_requested(controller) {
            return Ok(SchedulerOutcome::Paused { interrupted: false });
        }
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        tokio::task::spawn_blocking(move || store.mark_run_verifying(&run_id))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        progress(
            &controller.invocation,
            format!("Verifying workflow: {}", definition.name),
        )
        .await;
        verify_commands(controller, &definition.verification).await?;
    }
    await_boundary(
        controller,
        "run",
        &controller.id,
        definition.boundary,
        format!("Completed workflow: {}", definition.name),
    )
    .await?;
    finish_successful_run(controller, &definition).await?;
    Ok(SchedulerOutcome::Completed)
}

async fn finish_successful_run(
    controller: &Arc<RunController>,
    definition: &WorkflowDefinition,
) -> Result<(), String> {
    let cleanup_completed = {
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        tokio::task::spawn_blocking(move || store.run_cleanup_completed(&run_id))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
    };
    if !cleanup_completed {
        if !definition.cleanup.is_empty() {
            progress(
                &controller.invocation,
                format!("Cleaning up workflow: {}", definition.name),
            )
            .await;
            verify_commands(controller, &definition.cleanup)
                .await
                .map_err(|error| format!("workflow cleanup failed: {error}"))?;
        }
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        tokio::task::spawn_blocking(move || store.finish_successful_cleanup(&run_id))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
    }
    let set_run = (
        Arc::clone(&controller.store),
        controller.info.run_id.clone(),
    );
    tokio::task::spawn_blocking(move || set_run.0.set_run_state(&set_run.1, RunState::Completed))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    progress(
        &controller.invocation,
        format!("Completed workflow: {}", definition.name),
    )
    .await;
    Ok(())
}

async fn run_phase(
    controller: &Arc<RunController>,
    index: usize,
    total: usize,
) -> Result<SchedulerOutcome, String> {
    let phase = controller
        .definition
        .lock()
        .await
        .phases
        .get(index)
        .cloned()
        .ok_or_else(|| "scheduled phase missing definition".to_string())?;
    let store = Arc::clone(&controller.store);
    let run_id = controller.id.clone();
    let phase_name = phase.name.clone();
    tokio::task::spawn_blocking(move || store.mark_phase_running(&run_id, &phase_name))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    loop {
        if pause_requested(controller) {
            return Ok(SchedulerOutcome::Paused { interrupted: false });
        }
        ensure_task_records(controller, &phase.name).await?;
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        let name = phase.name.clone();
        let ready = tokio::task::spawn_blocking(move || store.ready_tasks(&run_id, &name))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let all = {
            let store = Arc::clone(&controller.store);
            let run_id = controller.id.clone();
            let name = phase.name.clone();
            tokio::task::spawn_blocking(move || store.scheduled_tasks(&run_id, &name))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
        };
        let metadata = {
            let store = Arc::clone(&controller.store);
            let run_id = controller.id.clone();
            let name = phase.name.clone();
            tokio::task::spawn_blocking(move || store.phase_metadata(&run_id, &name))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
        };
        if let Some(checkpoint) = all
            .iter()
            .find(|task| matches!(task.state.as_str(), "implemented" | "verified"))
            .cloned()
        {
            let details = {
                let store = Arc::clone(&controller.store);
                let task_id = checkpoint.task_id.clone();
                tokio::task::spawn_blocking(move || store.scheduler_task(&task_id))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?
            };
            let task_def = controller
                .definition
                .lock()
                .await
                .phases
                .get(index)
                .and_then(|phase| phase.tasks.iter().find(|task| task.name == details.name))
                .cloned()
                .ok_or_else(|| "recoverable task definition missing".to_string())?;
            let agent_id = {
                let store = Arc::clone(&controller.store);
                let task_id = checkpoint.task_id.clone();
                tokio::task::spawn_blocking(move || store.latest_task_agent(&task_id))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "recoverable task agent attribution missing".to_string())?
            };
            progress(
                &controller.invocation,
                format!("Resuming task from {}: {}", checkpoint.state, task_def.name),
            )
            .await;
            complete_task(
                controller,
                &checkpoint.task_id,
                &task_def,
                &agent_id,
                phase.review.is_some(),
                checkpoint.state == "verified",
            )
            .await
            .map_err(|(stage, error)| format!("{stage}: {error}"))?;
            continue;
        }
        if all.iter().all(|task| task.state == "integrated") {
            if !metadata.sealed {
                let mut activity = controller.activity_rx.clone();
                activity
                    .changed()
                    .await
                    .map_err(|_| "scheduler activity stopped".to_string())?;
                continue;
            }
            if !phase.verification.is_empty() {
                if pause_requested(controller) {
                    return Ok(SchedulerOutcome::Paused { interrupted: false });
                }
                let store = Arc::clone(&controller.store);
                let run_id = controller.id.clone();
                let name = phase.name.clone();
                tokio::task::spawn_blocking(move || store.mark_phase_verifying(&run_id, &name))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
                progress(
                    &controller.invocation,
                    format!("Verifying phase {}/{}: {}", index + 1, total, phase.name),
                )
                .await;
                if let Err(error) = verify_commands(controller, &phase.verification).await {
                    let store = Arc::clone(&controller.store);
                    let run_id = controller.id.clone();
                    let name = phase.name.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        store.mark_phase_failed(&run_id, &name)
                    })
                    .await;
                    return Err(error);
                }
            }
            if let Some(review) = phase.review.as_ref() {
                run_phase_review(controller, &phase, review, &all).await?;
            }
            await_boundary(
                controller,
                "phase",
                &phase.name,
                phase.boundary,
                format!("Completed phase: {}", phase.name),
            )
            .await?;
            if phase.review.is_some() {
                for task in &all {
                    close_task_agents(controller, &task.task_id).await?;
                    let store = Arc::clone(&controller.store);
                    let task_id = task.task_id.clone();
                    tokio::task::spawn_blocking(move || store.cleanup_task_worktree(&task_id))
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                }
            }
            let store = Arc::clone(&controller.store);
            let run_id = controller.id.clone();
            let name = phase.name.clone();
            tokio::task::spawn_blocking(move || store.mark_phase_completed(&run_id, &name))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            progress(
                &controller.invocation,
                format!("Completed phase {}/{}: {}", index + 1, total, phase.name),
            )
            .await;
            break Ok(SchedulerOutcome::Completed);
        }
        if ready.is_empty() {
            let mut activity = controller.activity_rx.clone();
            activity
                .changed()
                .await
                .map_err(|_| "scheduler activity stopped".to_string())?;
            continue;
        }
        let completed = join_all(ready.into_iter().map(|scheduled| {
            let controller = Arc::clone(controller);
            async move {
                let details = {
                    let store = Arc::clone(&controller.store);
                    let task_id = scheduled.task_id.clone();
                    tokio::task::spawn_blocking(move || store.scheduler_task(&task_id))
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?
                };
                let task_def = controller
                    .definition
                    .lock()
                    .await
                    .phases
                    .get(index)
                    .and_then(|phase| phase.tasks.iter().find(|task| task.name == details.name))
                    .cloned()
                    .ok_or_else(|| "scheduled task missing definition".to_string())?;
                let task_instructions =
                    prepare_task_instructions(&controller, &details.instructions, &task_def)
                        .await?;
                ensure_task_record(
                    &controller,
                    TaskDeclaration {
                        id: details.task_id.clone(),
                        name: details.name.clone(),
                        instructions: task_instructions.clone(),
                        read_scope: details.read_scope.clone(),
                        write_scope: details.write_scope.clone(),
                        verification: details.verification.clone(),
                    },
                )
                .await?;
                progress(
                    &controller.invocation,
                    format!("Running task: {}", task_def.name),
                )
                .await;
                let agent_def = controller
                    .definition
                    .lock()
                    .await
                    .agents
                    .get(&task_def.agent)
                    .cloned()
                    .ok_or_else(|| "agent missing".to_string())?;
                let agent = AgentSpec {
                    name: scheduler_agent_name("", &task_def.name, &scheduled.task_id),
                    display_name: Some(task_def.name.clone()),
                    instructions: "Implement the task requirements above.".into(),
                    profile: agent_def.profile,
                    model: agent_def.model,
                    reasoning_effort: agent_def.reasoning_effort.and_then(parse_effort),
                    tool_profile: agent_def.tool_profile,
                };
                let store = Arc::clone(&controller.store);
                let id = scheduled.task_id.clone();
                tokio::task::spawn_blocking(move || store.mark_task_running(&id))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
                let task_id = scheduled.task_id.clone();
                let operation =
                    run_task_handler(controller.invocation.clone(), task_id, agent).await;
                Ok::<_, String>((scheduled, task_def, operation))
            }
        }))
        .await;
        let mut retry_capacity = false;
        let mut first_error = None;
        for completed in completed {
            let (scheduled, task_def, operation) = completed?;
            let operation = match operation {
                Ok(operation) => operation,
                Err(error) => {
                    let store = Arc::clone(&controller.store);
                    let task_id = scheduled.task_id.clone();
                    if is_capacity_error(&error) {
                        let _ =
                            tokio::task::spawn_blocking(move || store.mark_task_ready(&task_id))
                                .await;
                        retry_capacity = true;
                    } else {
                        let _ =
                            tokio::task::spawn_blocking(move || store.mark_task_failed(&task_id))
                                .await;
                        progress(
                            &controller.invocation,
                            format!("Task {} failed during agent", task_def.name),
                        )
                        .await;
                        first_error.get_or_insert_with(|| error.to_string());
                    }
                    continue;
                }
            };
            let task_agent_id = operation
                .code_mode_result(&ToolPayload::Function {
                    arguments: serde_json::json!({"task_id": scheduled.task_id}).to_string(),
                })
                .get("agentId")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            let Some(task_agent_id) = task_agent_id else {
                let store = Arc::clone(&controller.store);
                let task_id = scheduled.task_id.clone();
                let _ = tokio::task::spawn_blocking(move || store.mark_task_failed(&task_id)).await;
                first_error.get_or_insert_with(|| {
                    format!("Task {} did not return an agent ID", task_def.name)
                });
                continue;
            };
            let store = Arc::clone(&controller.store);
            let task_id = scheduled.task_id.clone();
            tokio::task::spawn_blocking(move || store.mark_task_implemented(&task_id))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
            if let Err((stage, error)) = complete_task(
                controller,
                &scheduled.task_id,
                &task_def,
                &task_agent_id,
                phase.review.is_some(),
                false,
            )
            .await
            {
                progress(
                    &controller.invocation,
                    format!("Task {} failed during {stage}", task_def.name),
                )
                .await;
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if pause_requested(controller) {
            return Ok(SchedulerOutcome::Paused { interrupted: false });
        }
        if retry_capacity {
            continue;
        }
    }
}

fn pause_requested(controller: &RunController) -> bool {
    *controller.pause_rx.borrow()
}

async fn ensure_task_records(
    controller: &Arc<RunController>,
    phase_name: &str,
) -> Result<(), String> {
    let store = Arc::clone(&controller.store);
    let run_id = controller.id.clone();
    let phase_name = phase_name.to_string();
    let scheduled =
        tokio::task::spawn_blocking(move || store.scheduled_tasks(&run_id, &phase_name))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
    for scheduled_task in scheduled {
        let store = Arc::clone(&controller.store);
        let task_id = scheduled_task.task_id;
        let details = tokio::task::spawn_blocking(move || store.scheduler_task(&task_id))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let declaration = TaskDeclaration {
            id: details.task_id,
            name: details.name,
            instructions: details.instructions,
            read_scope: details.read_scope,
            write_scope: details.write_scope,
            verification: details.verification,
        };
        ensure_task_record(controller, declaration).await?;
    }
    Ok(())
}

const MAX_RENDERED_CONTEXT: usize = 64 * 1024;

async fn prepare_task_instructions(
    controller: &Arc<RunController>,
    base_instructions: &str,
    task: &TaskDefinition,
) -> Result<String, String> {
    let mut rendered = String::new();
    let mut seen = std::collections::BTreeSet::new();
    let packs = task
        .context
        .iter()
        .filter(|pack| seen.insert((*pack).clone()))
        .cloned()
        .collect::<Vec<_>>();
    let lifetimes = {
        let definition = controller.definition.lock().await;
        packs
            .iter()
            .filter_map(|pack| {
                definition
                    .context_packs
                    .get(pack)
                    .map(|definition| (pack.clone(), definition.lifetime))
            })
            .collect::<HashMap<_, _>>()
    };
    let resolved = join_all(packs.iter().map(|pack| {
        let controller = Arc::clone(controller);
        async move {
            resolve_or_collect_context(&controller, pack)
                .await
                .map(|resolved| (pack.clone(), resolved))
        }
    }))
    .await;
    for resolved in resolved {
        let (pack, resolved) = resolved?;
        if resolved.status != ContextPackStatus::Fresh {
            return Err(format!("context pack {pack} is not fresh after collection"));
        }
        if rendered.is_empty() {
            rendered.push_str("\n\n--- Flowdex context (bounded) ---\n");
        }
        for fragment in resolved.fragments {
            let entry = format!(
                "[{}/{} lines {}-{}]\n{}\n",
                pack, fragment.key, fragment.line_start, fragment.line_end, fragment.content
            );
            if rendered.len() + entry.len() > MAX_RENDERED_CONTEXT {
                rendered.push_str("[context truncated; additional fragments omitted]\n");
                rendered.push_str("--- End Flowdex context ---");
                return Ok(format!("{}{}", base_instructions, rendered));
            }
            rendered.push_str(&entry);
        }
    }
    if !rendered.is_empty() {
        rendered.push_str("--- End Flowdex context ---");
    }
    if packs
        .iter()
        .any(|pack| lifetimes.get(pack) == Some(&ContextPackLifetime::Repository))
    {
        rendered.push_str(
            "\n\nRepository context maintenance: if your edits invalidate the meaning of a received repository fragment, use publish_flowdex_context to supersede that fragment. Do not republish for incidental line changes that leave the context accurate; Flowdex commits deliberate updates with your task changes.",
        );
    }
    Ok(format!("{}{}", base_instructions, rendered))
}

async fn resolve_or_collect_context(
    controller: &Arc<RunController>,
    pack: &str,
) -> Result<ResolvedContextPack, String> {
    let gate = {
        let mut gates = controller.context_gates.lock().await;
        gates
            .entry(pack.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = gate.lock().await;
    let store = Arc::clone(&controller.store);
    let run_id = controller.id.clone();
    let pack = pack.to_string();
    let pack_for_resolve = pack.clone();
    let integration = controller.info.integration_worktree.clone();
    let mut resolved = tokio::task::spawn_blocking(move || {
        store.resolve_context_pack(&run_id, &pack_for_resolve, &integration)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    if resolved.status == ContextPackStatus::Fresh {
        return Ok(resolved);
    }
    progress(
        &controller.invocation,
        format!("Collecting context: {pack}"),
    )
    .await;
    for attempt in 0..2 {
        collect_context(controller, &pack, &resolved).await?;
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        let pack_for_resolve = pack.clone();
        let integration = controller.info.integration_worktree.clone();
        resolved = tokio::task::spawn_blocking(move || {
            store.resolve_context_pack(&run_id, &pack_for_resolve, &integration)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if resolved.status == ContextPackStatus::Fresh {
            progress(&controller.invocation, format!("Context ready: {pack}")).await;
            return Ok(resolved);
        }
        if attempt == 0 {
            progress(
                &controller.invocation,
                format!("Retrying context collection: {pack}"),
            )
            .await;
        }
    }
    let source = resolved.stale_sources.first().map(|source| {
        format!(
            "; stale fragment {} at {}:{}-{}",
            source.key,
            source.path.display(),
            source.line_start,
            source.line_end
        )
    });
    Err(format!(
        "context collectors completed without fresh pack {pack}{}",
        source.unwrap_or_default()
    ))
}

async fn collect_context(
    controller: &Arc<RunController>,
    pack: &str,
    resolved: &ResolvedContextPack,
) -> Result<(), String> {
    let definition = controller.definition.lock().await.clone();
    let pack_definition = definition
        .context_packs
        .get(pack)
        .cloned()
        .ok_or_else(|| format!("context pack {pack} is not declared"))?;
    let stale = if resolved.stale_sources.is_empty() {
        String::new()
    } else {
        format!(
            "\nStale sources:\n{}",
            resolved
                .stale_sources
                .iter()
                .map(format_stale_source)
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let repository_note = if pack_definition.lifetime == ContextPackLifetime::Repository {
        " The publish tool also updates the checked-in repository pack file; Flowdex will include it in the collector task commit."
    } else {
        ""
    };
    let instructions = format!(
        "Collect context pack `{pack}`.\n\n{}{}\n\nUse publish_flowdex_context to publish one or more fresh, bounded source-backed fragments. The collection is incomplete until the tool accepts a fresh fragment.{repository_note} Do not return source context through the orchestrator or finish with prose only.",
        pack_definition.instructions, stale,
    );
    let collector_suffix = Uuid::new_v4().simple().to_string();
    let task_id = format!("context-{collector_suffix}");
    let declaration = TaskDeclaration {
        id: task_id.clone(),
        name: format!("collect context {pack}"),
        instructions,
        read_scope: vec![],
        write_scope: vec![],
        verification: vec![],
    };
    let store = Arc::clone(&controller.store);
    let info = controller.info.clone();
    let declaration_for_store = declaration.clone();
    tokio::task::spawn_blocking(move || store.create_task(&info, &declaration_for_store))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let agent_definition = definition
        .agents
        .get(&pack_definition.agent)
        .cloned()
        .ok_or_else(|| format!("context pack agent {} is missing", pack_definition.agent))?;
    let agent = AgentSpec {
        name: format!("context_collector_{collector_suffix}"),
        display_name: Some(format!("{} context", pack)),
        instructions: format!(
            "Publish the requested bounded context with publish_flowdex_context before finishing.{repository_note}"
        ),
        profile: agent_definition.profile,
        model: agent_definition.model,
        reasoning_effort: agent_definition.reasoning_effort.and_then(parse_effort),
        tool_profile: agent_definition.tool_profile,
    };
    let result = run_task_handler(controller.invocation.clone(), task_id.clone(), agent).await;
    let cleanup = match close_task_agents(controller, &task_id).await {
        Ok(()) => task::integrate_task(controller.invocation.clone(), task_id)
            .await
            .map_err(|e| format!("context collector cleanup failed for {pack}: {e}")),
        Err(error) => Err(format!(
            "context collector cleanup failed for {pack}: {error}"
        )),
    };
    match result {
        Ok(_) => {
            cleanup?;
        }
        Err(error) => {
            let _ = cleanup;
            return Err(format!("context collector failed for {pack}: {error}"));
        }
    }
    Ok(())
}

async fn close_task_agents(controller: &RunController, task_id: &str) -> Result<(), String> {
    for agent_id in task::agents_for_task(task_id) {
        match controller
            .invocation
            .session
            .services
            .agent_control
            .close_agent(agent_id)
            .await
        {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.details(),
                    CodexErrorDetails::ThreadNotFound(_) | CodexErrorDetails::InternalAgentDied
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    task::forget_task_agents(task_id);
    Ok(())
}

fn format_stale_source(source: &ContextStaleSource) -> String {
    format!(
        "{}: {} ({}-{})",
        source.key,
        source.path.display(),
        source.line_start,
        source.line_end
    )
}

async fn ensure_task_record(
    controller: &Arc<RunController>,
    declaration: TaskDeclaration,
) -> Result<(), String> {
    let store = Arc::clone(&controller.store);
    let info = controller.info.clone();
    tokio::task::spawn_blocking(move || {
        let task_id = declaration.id.clone();
        match store.task(&task_id) {
            Ok(_) => Ok(()),
            Err(FlowdexStoreError::TaskNotFound(_)) => {
                store.create_task(&info, &declaration).map(|_| ())
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

async fn complete_task(
    controller: &Arc<RunController>,
    task_id: &str,
    task_def: &TaskDefinition,
    task_agent_id: &str,
    retain_worktree: bool,
    already_verified: bool,
) -> Result<(), (String, String)> {
    let mut task_agent_id = task_agent_id.to_string();
    if !already_verified && !task_def.verification.is_empty() {
        let mut repairs = 0usize;
        loop {
            progress(
                &controller.invocation,
                format!("Verifying task: {}", task_def.name),
            )
            .await;
            let verification_output =
                run_verify_handler(controller.invocation.clone(), task_id.to_string())
                    .await
                    .map_err(|error| ("verification".to_string(), error.to_string()))?;
            let verification_result =
                verification_output.code_mode_result(&ToolPayload::Function {
                    arguments: serde_json::json!({"task_id": task_id}).to_string(),
                });
            let passed = verification_result
                .get("passed")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            if passed {
                break;
            }
            if repairs >= task_def.verification_repair_limit {
                return Err((
                    "verification".to_string(),
                    format!("Task {} failed during verification", task_def.name),
                ));
            }
            repairs += 1;
            let store = Arc::clone(&controller.store);
            let run_id = controller.id.clone();
            let id = task_id.to_string();
            tokio::task::spawn_blocking(move || {
                store.increment_verification_repairs("task", &run_id, &id)
            })
            .await
            .map_err(|error| ("verification repair".to_string(), error.to_string()))?
            .map_err(|error| ("verification repair".to_string(), error.to_string()))?;
            progress(
                &controller.invocation,
                format!(
                    "Repairing verification: {} ({repairs}/{})",
                    task_def.name, task_def.verification_repair_limit
                ),
            )
            .await;
            let repair_instructions = format!(
                "Flowdex ran the declared task verification and it failed. Repair the failure while preserving the original requirements. Do not run the declared verification commands yourself; Flowdex will rerun them automatically after your repair. Create a new commit; do not amend, rebase, reset, or rewrite existing commits.\n\nVerification result:\n{}",
                serde_json::to_string_pretty(&verification_result)
                    .unwrap_or_else(|_| verification_result.to_string())
            );
            if resume_task_agent(controller, &task_agent_id, repair_instructions.clone())
                .await
                .is_err()
            {
                progress(
                    &controller.invocation,
                    format!("Starting replacement repair agent: {}", task_def.name),
                )
                .await;
                task_agent_id =
                    spawn_recovery_task_agent(controller, task_id, task_def, repair_instructions)
                        .await
                        .map_err(|error| ("verification repair".to_string(), error))?;
            }
        }
        let store = Arc::clone(&controller.store);
        let id = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            store.set_scheduler_task_state(&id, SchedulerTaskState::Verified)
        })
        .await
        .map_err(|error| ("verification".to_string(), error.to_string()))?
        .map_err(|error| ("verification".to_string(), error.to_string()))?;
    }
    if let Some(review) = task_def.review.as_ref() {
        review_task(controller, task_id, task_def, review, None, "task", task_id)
            .await
            .map_err(|error| ("review".to_string(), error))?;
    }
    progress(
        &controller.invocation,
        format!("Integrating task: {}", task_def.name),
    )
    .await;
    if !retain_worktree {
        close_task_agents(controller, task_id)
            .await
            .map_err(|error| ("integration".to_string(), error))?;
    }
    run_integrate_handler(
        controller.invocation.clone(),
        task_id.to_string(),
        retain_worktree,
    )
    .await
    .map_err(|error| ("integration".to_string(), error.to_string()))?;
    await_boundary(
        controller,
        "task",
        task_id,
        task_def.boundary,
        format!("Completed task: {}", task_def.name),
    )
    .await
    .map_err(|error| ("boundary".to_string(), error))?;
    let store = Arc::clone(&controller.store);
    let id = task_id.to_string();
    tokio::task::spawn_blocking(move || store.mark_task_integrated(&id))
        .await
        .map_err(|error| ("integration".to_string(), error.to_string()))?
        .map_err(|error| ("integration".to_string(), error.to_string()))?;
    progress(
        &controller.invocation,
        format!("Completed task: {}", task_def.name),
    )
    .await;
    Ok(())
}

async fn await_boundary(
    controller: &Arc<RunController>,
    scope_kind: &str,
    scope_id: &str,
    boundary: Boundary,
    reason: String,
) -> Result<(), String> {
    if boundary == Boundary::Continue {
        progress(
            &controller.invocation,
            format!("Boundary continued: {scope_kind} {scope_id}"),
        )
        .await;
        return Ok(());
    }
    let pending = PendingBoundary {
        run_id: controller.id.clone(),
        scope_kind: scope_kind.to_string(),
        scope_id: scope_id.to_string(),
        target: match boundary {
            Boundary::Continue => "continue",
            Boundary::Orchestrator => "orchestrator",
            Boundary::Human => "human",
        }
        .to_string(),
        reason,
        transition: "awaiting_continuation".to_string(),
    };
    super::publish_flowdex_boundary(Arc::clone(&controller.store), pending.clone()).await?;
    progress(
        &controller.invocation,
        format!("Boundary: {} {}", pending.scope_kind, pending.scope_id),
    )
    .await;
    super::wait_flowdex_boundary_continuation(
        &pending.run_id,
        &pending.scope_kind,
        &pending.scope_id,
    )
    .await;
    Ok(())
}

async fn run_phase_review(
    controller: &Arc<RunController>,
    phase: &PhaseDefinition,
    review: &ReviewDefinition,
    tasks: &[codex_flowdex::store::ScheduledTask],
) -> Result<(), String> {
    let mut reviewer_thread_id = None;
    let result =
        run_phase_review_rounds(controller, phase, review, tasks, &mut reviewer_thread_id).await;
    let close = close_review_agent(controller, reviewer_thread_id.as_deref()).await;
    match result {
        Err(error) => Err(error),
        Ok(()) => close,
    }
}

async fn run_phase_review_rounds(
    controller: &Arc<RunController>,
    phase: &PhaseDefinition,
    review: &ReviewDefinition,
    tasks: &[codex_flowdex::store::ScheduledTask],
    reviewer_thread_id: &mut Option<String>,
) -> Result<(), String> {
    let first = tasks
        .first()
        .ok_or_else(|| "phase review requires a task".to_string())?;
    let reviewer = controller
        .definition
        .lock()
        .await
        .agents
        .get(&review.agent)
        .cloned()
        .ok_or_else(|| format!("review agent {} is missing", review.agent))?;

    for _ in 0..review.max_rounds {
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        let phase_name = phase.name.clone();
        let round = tokio::task::spawn_blocking(move || {
            store.increment_phase_review_rounds(&run_id, &phase_name)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())? as u32;
        if round > review.max_rounds {
            return phase_review_boundary(
                controller,
                phase,
                format!("Phase review exhausted after {} rounds", review.max_rounds),
            )
            .await;
        }
        progress(
            &controller.invocation,
            format!(
                "Reviewing phase: {} (round {round}/{})",
                phase.name, review.max_rounds
            ),
        )
        .await;

        let base_commit = {
            let store = Arc::clone(&controller.store);
            let task_id = first.task_id.clone();
            tokio::task::spawn_blocking(move || store.task(&task_id))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
                .base_commit
        };
        let diff = phase_diff(controller, &base_commit).await?;
        let operation = codex_flowdex::store::ReviewOperation {
            operation_id: Uuid::new_v4().to_string(),
            run_id: controller.id.clone(),
            scope_kind: "phase".into(),
            scope_id: phase.name.clone(),
            round: round as i64,
            reviewer_thread_id: reviewer_thread_id.clone().unwrap_or_default(),
            state: "pending".into(),
        };
        let operation_id = operation.operation_id.clone();
        let store = Arc::clone(&controller.store);
        let operation_for_store = operation.clone();
        tokio::task::spawn_blocking(move || store.record_review_operation(&operation_for_store))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let reviewer_agent = AgentSpec {
            name: scheduler_agent_name("review", &phase.name, &operation_id),
            display_name: Some(format!("{} reviewer", phase.name)),
            instructions: format!(
                "{}\n\nReview the integrated phase diff below against the phase requirements and verification result. Your review is incomplete until report_flowdex_review accepts exactly one report; submit an empty findings array when it passes. Do not finish with prose only or message a worker directly.\n\nPhase requirements:\n{}\n\nIntegrated diff:\n{}",
                review.instructions, phase.instructions, diff
            ),
            profile: reviewer.profile.clone(),
            model: reviewer.model.clone(),
            reasoning_effort: reviewer.reasoning_effort.clone().and_then(parse_effort),
            tool_profile: reviewer.tool_profile.clone(),
        };
        if let Some(existing_id) = reviewer_thread_id.as_deref() {
            resume_review_agent(
                controller,
                existing_id,
                reviewer_agent.instructions,
                operation.clone(),
            )
            .await?;
        } else {
            let reviewer_result = task::run_task_agent_with_review(
                controller.invocation.clone(),
                first.task_id.clone(),
                reviewer_agent,
                task::ReviewDispatch {
                    operation: operation.clone(),
                    store: Arc::clone(&controller.store),
                    worktree: Some(controller.info.integration_worktree.clone()),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            let spawned_id = reviewer_result
                .code_mode_result(&ToolPayload::Function {
                    arguments: serde_json::json!({"task_id": first.task_id}).to_string(),
                })
                .get("agentId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if spawned_id.is_empty() {
                return Err("review agent did not return an id".into());
            }
            *reviewer_thread_id = Some(spawned_id.clone());
            let store = Arc::clone(&controller.store);
            let mut accepted = operation.clone();
            accepted.reviewer_thread_id = spawned_id;
            accepted.state = "pending".into();
            tokio::task::spawn_blocking(move || store.record_review_operation(&accepted))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        }
        let store = Arc::clone(&controller.store);
        let accepted = tokio::task::spawn_blocking({
            let id = operation_id.clone();
            move || store.review_operation(&id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if accepted.state != "accepted" {
            return Err("review agent completed without a report".into());
        }
        let store = Arc::clone(&controller.store);
        let findings = tokio::task::spawn_blocking({
            let id = operation_id.clone();
            move || store.review_findings(&id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if findings.is_empty() {
            return Ok(());
        }
        let mut attributed = Vec::with_capacity(findings.len());
        for finding in findings {
            let head = integration_head(&controller.info.integration_worktree).await?;
            let store = Arc::clone(&controller.store);
            let finding_id = finding.finding_id.clone();
            let integration = controller.info.integration_worktree.clone();
            let attribution = tokio::task::spawn_blocking(move || {
                store.attribute_review_finding(&finding_id, &integration, &head)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
            let Some(attribution) = attribution else {
                return phase_review_boundary(
                    controller,
                    phase,
                    format!("Unattributed phase review finding: {}", finding.reason),
                )
                .await;
            };
            attributed.push((finding, attribution));
        }
        let grouped = group_phase_findings(attributed);
        if round == review.max_rounds {
            return phase_review_boundary(
                controller,
                phase,
                format!("Phase review exhausted after {} rounds", review.max_rounds),
            )
            .await;
        }
        let mut grouped = grouped.into_iter().collect::<Vec<_>>();
        grouped.sort_by(|left, right| left.0.cmp(&right.0));
        for (task_id, task_findings) in grouped {
            repair_phase_task(controller, phase, &task_id, &task_findings).await?;
        }
        if !phase.verification.is_empty() {
            verify_commands(controller, &phase.verification).await?;
        }
    }
    Ok(())
}

fn group_phase_findings(
    findings: Vec<(ReviewFinding, codex_flowdex::store::ReviewAttribution)>,
) -> HashMap<String, Vec<(ReviewFinding, codex_flowdex::store::ReviewAttribution)>> {
    let mut grouped = HashMap::new();
    for (finding, attribution) in findings {
        grouped
            .entry(attribution.task_id.clone())
            .or_insert_with(Vec::new)
            .push((finding, attribution));
    }
    grouped
}

async fn repair_phase_task(
    controller: &Arc<RunController>,
    phase: &PhaseDefinition,
    task_id: &str,
    findings: &[(ReviewFinding, codex_flowdex::store::ReviewAttribution)],
) -> Result<(), String> {
    progress(
        &controller.invocation,
        format!("Repairing phase review: {}", task_id),
    )
    .await;
    let details = {
        let store = Arc::clone(&controller.store);
        let id = task_id.to_string();
        tokio::task::spawn_blocking(move || store.scheduler_task(&id))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?
    };
    let task_def = controller
        .definition
        .lock()
        .await
        .phases
        .iter()
        .find(|candidate| candidate.name == phase.name)
        .and_then(|candidate| {
            candidate
                .tasks
                .iter()
                .find(|task| task.name == details.name)
        })
        .cloned()
        .ok_or_else(|| "phase repair task definition missing".to_string())?;
    let repair = findings
        .iter()
        .map(|(finding, _)| {
            format!(
                "{}:{}-{}: {}",
                finding.file, finding.line_start, finding.line_end, finding.reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let agent_id = findings
        .first()
        .map(|(_, attribution)| attribution.agent_id.as_str())
        .ok_or_else(|| "phase repair finding attribution missing".to_string())?;
    resume_task_agent(
        controller,
        agent_id,
        format!(
            "Repair these phase review findings while preserving the original task requirements. Do not run the declared verification commands yourself; Flowdex will run them automatically after your repair:\n{repair}"
        ),
    )
    .await?;
    if !task_def.verification.is_empty() {
        let output = run_verify_handler(controller.invocation.clone(), task_id.to_string())
            .await
            .map_err(|e| e.to_string())?;
        if output
            .code_mode_result(&ToolPayload::Function {
                arguments: serde_json::json!({"task_id": task_id}).to_string(),
            })
            .get("passed")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(format!(
                "phase repair failed verification for {}",
                task_def.name
            ));
        }
    }
    let integrated =
        run_integrate_handler(controller.invocation.clone(), task_id.to_string(), true)
            .await
            .map_err(|e| e.to_string())?;
    let commits = integrated
        .code_mode_result(&ToolPayload::Function {
            arguments: serde_json::json!({"task_id": task_id}).to_string(),
        })
        .get("commits")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| "phase repair integration omitted commits".to_string())?;
    for (finding, _) in findings {
        for commit in &commits {
            let source_commit = commit
                .get("sourceCommit")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "phase repair commit omitted sourceCommit".to_string())?;
            let integrated_commit = commit
                .get("integratedCommit")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let store = Arc::clone(&controller.store);
            let task_id = task_id.to_string();
            let source = source_commit.to_string();
            let operation_id =
                tokio::task::spawn_blocking(move || store.task_commit_operation(&task_id, &source))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
            let resolution = ReviewResolution {
                finding_id: finding.finding_id.clone(),
                repair_operation_id: operation_id,
                source_commit: source_commit.to_string(),
                integrated_commit,
            };
            let store = Arc::clone(&controller.store);
            tokio::task::spawn_blocking(move || store.record_review_resolution(&resolution))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

async fn phase_review_boundary(
    controller: &Arc<RunController>,
    phase: &PhaseDefinition,
    reason: String,
) -> Result<(), String> {
    let boundary = if phase.boundary == Boundary::Human {
        Boundary::Human
    } else {
        Boundary::Orchestrator
    };
    await_boundary(controller, "phase", &phase.name, boundary, reason).await
}

async fn integration_head(path: &std::path::Path) -> Result<String, String> {
    tokio::task::spawn_blocking({
        let path = path.to_path_buf();
        move || {
            Command::new("git")
                .current_dir(path)
                .args(["rev-parse", "HEAD"])
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn phase_diff(controller: &Arc<RunController>, base_commit: &str) -> Result<String, String> {
    let path = controller.info.integration_worktree.clone();
    let base = base_commit.to_string();
    tokio::task::spawn_blocking(move || {
        Command::new("git")
            .current_dir(path)
            .args(["diff", &base, "HEAD"])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .chars()
                    .take(64 * 1024)
                    .collect()
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn review_task(
    controller: &Arc<RunController>,
    task_id: &str,
    task_def: &TaskDefinition,
    review: &ReviewDefinition,
    review_worktree: Option<std::path::PathBuf>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<(), String> {
    let mut reviewer_thread_id = None;
    let result = review_task_rounds(
        controller,
        task_id,
        task_def,
        review,
        review_worktree,
        scope_kind,
        scope_id,
        &mut reviewer_thread_id,
    )
    .await;
    let close = close_review_agent(controller, reviewer_thread_id.as_deref()).await;
    match result {
        Err(error) => Err(error),
        Ok(()) => close,
    }
}

async fn review_task_rounds(
    controller: &Arc<RunController>,
    task_id: &str,
    task_def: &TaskDefinition,
    review: &ReviewDefinition,
    review_worktree: Option<std::path::PathBuf>,
    scope_kind: &str,
    scope_id: &str,
    reviewer_thread_id: &mut Option<String>,
) -> Result<(), String> {
    let reviewer = controller
        .definition
        .lock()
        .await
        .agents
        .get(&review.agent)
        .cloned()
        .ok_or_else(|| format!("review agent {} is missing", review.agent))?;
    let task = {
        let store = Arc::clone(&controller.store);
        let id = task_id.to_string();
        tokio::task::spawn_blocking(move || store.task(&id))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?
    };
    for _ in 0..review.max_rounds {
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        let id = scope_id.to_string();
        let scope_kind_for_counter = scope_kind.to_string();
        let round = tokio::task::spawn_blocking(move || {
            store.increment_review_rounds(&scope_kind_for_counter, &run_id, &id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())? as u32;
        if round > review.max_rounds {
            return task_review_boundary(
                controller,
                task_def,
                task_id,
                format!("Task review exhausted after {} rounds", review.max_rounds),
            )
            .await;
        }
        let diff = tokio::task::spawn_blocking({
            let path = review_worktree
                .clone()
                .unwrap_or_else(|| task.worktree_path.clone());
            let base = task.base_commit.clone();
            move || {
                Command::new("git")
                    .current_dir(path)
                    .args(["diff", &base, "HEAD"])
                    .output()
                    .map(|output| {
                        String::from_utf8_lossy(&output.stdout)
                            .chars()
                            .take(64 * 1024)
                            .collect::<String>()
                    })
                    .map_err(|e| e.to_string())
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        let operation = codex_flowdex::store::ReviewOperation {
            operation_id: Uuid::new_v4().to_string(),
            run_id: controller.id.clone(),
            scope_kind: scope_kind.into(),
            scope_id: scope_id.into(),
            round: round as i64,
            reviewer_thread_id: reviewer_thread_id.clone().unwrap_or_default(),
            state: "pending".into(),
        };
        let operation_id = operation.operation_id.clone();
        let store = Arc::clone(&controller.store);
        tokio::task::spawn_blocking({
            let operation = operation.clone();
            move || store.record_review_operation(&operation)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let agent = AgentSpec {
            name: scheduler_agent_name("review", &task_def.name, &operation_id),
            display_name: Some(format!("{} reviewer", task_def.name)),
            instructions: format!(
                "{}\n\nReview the committed task diff below against the task requirements and verification result. Your review is incomplete until report_flowdex_review accepts exactly one report; submit an empty findings array when it passes. Do not finish with prose only or message the worker directly.\n\nTask requirements:\n{}\n\nCommitted diff:\n{}",
                review.instructions, task.instructions, diff
            ),
            profile: reviewer.profile.clone(),
            model: reviewer.model.clone(),
            reasoning_effort: reviewer.reasoning_effort.clone().and_then(parse_effort),
            tool_profile: reviewer.tool_profile.clone(),
        };
        if let Some(existing_id) = reviewer_thread_id.as_deref() {
            resume_review_agent(
                controller,
                existing_id,
                agent.instructions,
                operation.clone(),
            )
            .await?;
        } else {
            let reviewer_result = task::run_task_agent_with_review(
                controller.invocation.clone(),
                task_id.to_string(),
                agent,
                task::ReviewDispatch {
                    operation: codex_flowdex::store::ReviewOperation {
                        operation_id: operation_id.clone(),
                        ..operation.clone()
                    },
                    store: Arc::clone(&controller.store),
                    worktree: review_worktree.clone(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            let spawned_id = reviewer_result
                .code_mode_result(&ToolPayload::Function {
                    arguments: serde_json::json!({"task_id": task_id}).to_string(),
                })
                .get("agentId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if spawned_id.is_empty() {
                return Err("review agent did not return an id".into());
            }
            *reviewer_thread_id = Some(spawned_id.clone());
            let store = Arc::clone(&controller.store);
            let mut updated = operation.clone();
            updated.reviewer_thread_id = spawned_id;
            updated.state = "pending".to_string();
            tokio::task::spawn_blocking(move || store.record_review_operation(&updated))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        }
        let store = Arc::clone(&controller.store);
        let accepted = tokio::task::spawn_blocking({
            let id = operation_id.clone();
            move || store.review_operation(&id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if accepted.state != "accepted" {
            return Err("review agent completed without a report".into());
        }
        let store = Arc::clone(&controller.store);
        let findings = tokio::task::spawn_blocking({
            let id = operation_id.clone();
            move || store.review_findings(&id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if findings.is_empty() {
            return Ok(());
        }
        let review_path = review_worktree
            .clone()
            .unwrap_or_else(|| task.worktree_path.clone());
        let review_head = integration_head(&review_path).await?;
        let mut attributed = Vec::with_capacity(findings.len());
        for finding in findings {
            let store = Arc::clone(&controller.store);
            let finding_id = finding.finding_id.clone();
            let path = review_path.clone();
            let head = review_head.clone();
            let attribution = tokio::task::spawn_blocking(move || {
                store.attribute_review_finding(&finding_id, &path, &head)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
            let Some(attribution) = attribution else {
                return task_review_boundary(
                    controller,
                    task_def,
                    task_id,
                    format!("Unattributed task review finding: {}", finding.reason),
                )
                .await;
            };
            attributed.push((finding, attribution));
        }
        if round == review.max_rounds {
            return task_review_boundary(
                controller,
                task_def,
                task_id,
                format!("Task review exhausted after {} rounds", review.max_rounds),
            )
            .await;
        }
        let repair = attributed
            .iter()
            .map(|(finding, _)| finding)
            .map(|finding| {
                format!(
                    "{}:{}-{}: {}",
                    finding.file, finding.line_start, finding.line_end, finding.reason
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let agent_id = attributed[0].1.agent_id.clone();
        resume_task_agent(
            controller,
            &agent_id,
            format!(
                "Repair these review findings while preserving the original task requirements. Do not run the declared verification commands yourself; Flowdex will run them automatically after your repair. Create a new commit; do not amend, rebase, reset, or rewrite existing commits:\n{repair}"
            ),
        )
        .await?;
        if !task_def.verification.is_empty() {
            let output = run_verify_handler(controller.invocation.clone(), task_id.to_string())
                .await
                .map_err(|e| e.to_string())?;
            if output
                .code_mode_result(&ToolPayload::Function {
                    arguments: serde_json::json!({"task_id": task_id}).to_string(),
                })
                .get("passed")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
            {
                return Err("review repair failed verification".into());
            }
        }
        let repair_commits = task_source_commits(&task.worktree_path, &task.base_commit).await?;
        if let Some(source_commit) = repair_commits.last() {
            let operation_id = {
                let store = Arc::clone(&controller.store);
                let task_id = task_id.to_string();
                let source = source_commit.clone();
                tokio::task::spawn_blocking(move || store.task_commit_operation(&task_id, &source))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?
            };
            for (finding, _) in &attributed {
                let resolution = ReviewResolution {
                    finding_id: finding.finding_id.clone(),
                    repair_operation_id: operation_id.clone(),
                    source_commit: source_commit.clone(),
                    integrated_commit: None,
                };
                let store = Arc::clone(&controller.store);
                tokio::task::spawn_blocking(move || store.record_review_resolution(&resolution))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

async fn task_review_boundary(
    controller: &Arc<RunController>,
    task: &TaskDefinition,
    task_id: &str,
    reason: String,
) -> Result<(), String> {
    let boundary = if task.boundary == Boundary::Human {
        Boundary::Human
    } else {
        Boundary::Orchestrator
    };
    await_boundary(controller, "task", task_id, boundary, reason).await
}

async fn resume_task_agent(
    controller: &Arc<RunController>,
    agent_id: &str,
    instructions: String,
) -> Result<(), String> {
    let mut invocation = controller.invocation.clone();
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({
            "agent_id": agent_id,
            "instructions": instructions,
            "options": {"context_mode": "keep"},
        })
        .to_string(),
    };
    FlowdexResumeAgentHandler
        .handle(invocation)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn resume_review_agent(
    controller: &Arc<RunController>,
    agent_id: &str,
    instructions: String,
    operation: codex_flowdex::store::ReviewOperation,
) -> Result<(), String> {
    let thread_id = ThreadId::from_string(agent_id).map_err(|error| error.to_string())?;
    let agent_path = controller
        .invocation
        .session
        .services
        .agent_control
        .get_agent_metadata(thread_id)
        .and_then(|metadata| metadata.agent_path)
        .ok_or_else(|| format!("review agent {agent_id} has no agent path"))?;
    super::review::activate_review_agent(
        agent_path.clone(),
        operation,
        Arc::clone(&controller.store),
    );
    let result = resume_task_agent(controller, agent_id, instructions).await;
    super::review::deactivate_review_agent(&agent_path);
    result
}

async fn close_review_agent(
    controller: &RunController,
    agent_id: Option<&str>,
) -> Result<(), String> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };
    let thread_id = ThreadId::from_string(agent_id).map_err(|error| error.to_string())?;
    match controller
        .invocation
        .session
        .services
        .agent_control
        .close_agent(thread_id)
        .await
    {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.details(),
                CodexErrorDetails::ThreadNotFound(_) | CodexErrorDetails::InternalAgentDied
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn task_source_commits(
    worktree: &std::path::Path,
    base_commit: &str,
) -> Result<Vec<String>, String> {
    let worktree = worktree.to_path_buf();
    let base_commit = base_commit.to_string();
    tokio::task::spawn_blocking(move || {
        Command::new("git")
            .current_dir(worktree)
            .args(["rev-list", "--reverse", &format!("{base_commit}..HEAD")])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn is_capacity_error(error: &FunctionCallError) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("agent thread limit reached")
}

fn scheduler_agent_name(role: &str, display_name: &str, unique_id: &str) -> String {
    let mut slug = String::new();
    for ch in display_name.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            slug.push(ch);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
        if slug.len() == 48 {
            break;
        }
    }
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "unnamed" } else { slug };
    let suffix: String = unique_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    if role.is_empty() {
        format!("{slug}_{suffix}")
    } else {
        format!("{role}_{slug}_{suffix}")
    }
}

fn parse_effort(value: String) -> Option<ReasoningEffort> {
    Some(match value.as_str() {
        "none" => ReasoningEffort::None,
        "minimal" => ReasoningEffort::Minimal,
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "high" => ReasoningEffort::High,
        "xhigh" => ReasoningEffort::XHigh,
        "max" => ReasoningEffort::Max,
        "ultra" => ReasoningEffort::Ultra,
        _ => ReasoningEffort::Custom(value),
    })
}

async fn run_task_handler(
    mut invocation: ToolInvocation,
    task_id: String,
    agent: AgentSpec,
) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({
            "task_id": task_id,
            "agent": {
                "name": agent.name,
                "display_name": agent.display_name,
                "instructions": agent.instructions,
                "profile": agent.profile,
                "tool_profile": agent.tool_profile,
                "model": agent.model,
                "reasoning_effort": agent.reasoning_effort,
            }
        })
        .to_string(),
    };
    FlowdexTaskRunAgentHandler.handle(invocation).await
}

async fn spawn_recovery_task_agent(
    controller: &Arc<RunController>,
    task_id: &str,
    task_def: &TaskDefinition,
    instructions: String,
) -> Result<String, String> {
    let definition = controller
        .definition
        .lock()
        .await
        .agents
        .get(&task_def.agent)
        .cloned()
        .ok_or_else(|| format!("task agent {} is missing", task_def.agent))?;
    let agent = AgentSpec {
        name: scheduler_agent_name("", &format!("{} repair", task_def.name), task_id),
        display_name: Some(format!("{} repair", task_def.name)),
        instructions,
        profile: definition.profile,
        model: definition.model,
        reasoning_effort: definition.reasoning_effort.and_then(parse_effort),
        tool_profile: definition.tool_profile,
    };
    let output = run_task_handler(controller.invocation.clone(), task_id.to_string(), agent)
        .await
        .map_err(|error| error.to_string())?;
    output
        .code_mode_result(&ToolPayload::Function {
            arguments: serde_json::json!({"task_id": task_id}).to_string(),
        })
        .get("agentId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "replacement repair agent did not return an agent ID".to_string())
}

async fn run_verify_handler(
    mut invocation: ToolInvocation,
    task_id: String,
) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({"task_id": task_id}).to_string(),
    };
    FlowdexTaskVerifyHandler.handle(invocation).await
}

async fn run_integrate_handler(
    mut invocation: ToolInvocation,
    task_id: String,
    retain_worktree: bool,
) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
    if retain_worktree {
        return task::integrate_task_retained(invocation, task_id)
            .await
            .map(|output| Box::new(output) as Box<dyn ToolOutput>);
    }
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({"task_id": task_id}).to_string(),
    };
    FlowdexTaskIntegrateHandler.handle(invocation).await
}

async fn verify_commands(
    controller: &Arc<RunController>,
    commands: &[String],
) -> Result<(), String> {
    let invocation = {
        let mut invocation = controller.invocation.clone();
        invocation.payload = ToolPayload::Function {
            arguments: serde_json::json!({
                "commands": commands,
                "workdir": controller.info.integration_worktree.to_string_lossy(),
            })
            .to_string(),
        };
        invocation
    };
    let verifier = FlowdexVerifyHandler::new(shell_command_backend_for_features(
        controller.invocation.turn.config.features.get(),
    ));
    let output = verifier
        .handle(invocation.clone())
        .await
        .map_err(|e| e.to_string())?;
    if output
        .code_mode_result(&invocation.payload)
        .get("passed")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        Ok(())
    } else {
        Err("verification failed".to_string())
    }
}

async fn progress(invocation: &ToolInvocation, text: String) {
    let item = TurnItem::Reasoning(ReasoningItem {
        id: Uuid::new_v4().to_string(),
        summary_text: vec![text],
        raw_content: vec![],
    });
    invocation
        .session
        .emit_flowdex_progress(&invocation.turn, item)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, order: i64) -> ReviewFinding {
        ReviewFinding {
            finding_id: id.into(),
            operation_id: "review".into(),
            finding_order: order,
            file: "src/lib.rs".into(),
            line_start: order,
            line_end: order,
            reason: "repair".into(),
            rule_key: None,
            ast_grep_suitable: false,
            attributed_task_id: None,
            attributed_operation_id: None,
            attributed_agent_id: None,
        }
    }

    fn attribution(
        finding_id: &str,
        task_id: &str,
        operation_id: &str,
    ) -> codex_flowdex::store::ReviewAttribution {
        codex_flowdex::store::ReviewAttribution {
            finding_id: finding_id.into(),
            task_id: task_id.into(),
            operation_id: operation_id.into(),
            agent_id: format!("agent-{task_id}"),
            source_commit: format!("source-{task_id}"),
            integrated_commit: format!("integrated-{task_id}"),
        }
    }

    #[test]
    fn phase_review_groups_findings_by_attributed_task() {
        let grouped = group_phase_findings(vec![
            (
                finding("f-alpha", 1),
                attribution("f-alpha", "task-alpha", "op-alpha"),
            ),
            (
                finding("f-beta", 2),
                attribution("f-beta", "task-beta", "op-beta"),
            ),
            (
                finding("f-beta-2", 3),
                attribution("f-beta-2", "task-beta", "op-beta-2"),
            ),
        ]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped["task-alpha"]
                .iter()
                .map(|(finding, _)| finding.finding_id.as_str())
                .collect::<Vec<_>>(),
            vec!["f-alpha"]
        );
        assert_eq!(
            grouped["task-beta"]
                .iter()
                .map(|(finding, _)| finding.finding_id.as_str())
                .collect::<Vec<_>>(),
            vec!["f-beta", "f-beta-2"]
        );
    }

    #[test]
    fn scheduler_agent_names_are_valid_and_distinct() {
        let first = scheduler_agent_name("", "Use Context / Parser", "12345678-first");
        let second = scheduler_agent_name("", "Use Context / Parser", "87654321-second");

        assert_eq!(first, "use_context_parser_12345678");
        assert_eq!(second, "use_context_parser_87654321");
        assert!(
            first
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        );
    }
}
