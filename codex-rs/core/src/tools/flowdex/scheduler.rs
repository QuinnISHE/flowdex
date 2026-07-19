use super::task::{self, AgentSpec};
use super::verification::FlowdexVerifyHandler;
use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload, boxed_tool_output};
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::{
    FlowdexTaskIntegrateHandler, FlowdexTaskRunAgentHandler, FlowdexTaskVerifyHandler,
};
use crate::tools::registry::{CoreToolRuntime, ToolExecutor};
use codex_flowdex::store::{
    FlowdexStore, FlowdexStoreError, RunInfo, RunState, SchedulerTaskState, TaskDeclaration,
};
use codex_flowdex::{PhaseDefinition, TaskDefinition, WorkflowDefinition};
use codex_protocol::items::{ReasoningItem, TurnItem};
use codex_protocol::openai_models::ReasoningEffort;
use codex_tools::shell_command_backend_for_features;
use codex_tools::{JsonSchema, ResponsesApiTool, ToolName, ToolSpec};
use futures::future::join_all;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

const START: &str = "flowdex_start_run";
const QUEUE: &str = "flowdex_queue_task";
const SEAL: &str = "flowdex_seal_phase";
const WAIT: &str = "flowdex_wait_run";
const DIRECT_QUEUE: &str = "queue_flowdex_task";
const DIRECT_SEAL: &str = "seal_flowdex_phase";

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
                })
                .collect(),
            verification: w.verification,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RunStatus {
    Running,
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
}

static RUNS: OnceLock<Mutex<HashMap<String, Arc<RunController>>>> = OnceLock::new();
fn runs() -> &'static Mutex<HashMap<String, Arc<RunController>>> {
    RUNS.get_or_init(Default::default)
}

pub(crate) struct FlowdexStartRunHandler;
pub(crate) struct FlowdexQueueTaskHandler;
pub(crate) struct FlowdexSealPhaseHandler;
pub(crate) struct FlowdexWaitRunHandler;
pub(crate) struct QueueFlowdexTaskHandler;
pub(crate) struct SealFlowdexPhaseHandler;

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
    let (status, status_rx) = watch::channel(RunStatus::Running);
    let (activity, activity_rx) = watch::channel(0u64);
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
    });
    runs()
        .lock()
        .await
        .insert(run_id.clone(), Arc::clone(&controller));
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
    tokio::task::spawn_blocking(move || store.create_task(&info, &declaration))
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let _ = controller
        .activity
        .send(controller.activity_rx.borrow().wrapping_add(1));
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
    let _ = controller
        .activity
        .send(controller.activity_rx.borrow().wrapping_add(1));
    Ok(task::JsonOutput(
        serde_json::json!({"runId": args.run_id, "phase": args.phase}),
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
            RunStatus::Running => {
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
                return Err(FunctionCallError::RespondToModel(error));
            }
        }
    }
}

async fn remove_run_controller(run_id: &str) {
    if let Some(controller) = runs().lock().await.remove(run_id) {
        let _ = tokio::task::spawn_blocking(move || drop(controller)).await;
    }
}

async fn run_scheduler(controller: Arc<RunController>) {
    let result = tokio::select! {
        _ = controller.invocation.cancellation_token.cancelled() => {
            Err("Flowdex run cancelled".to_string())
        }
        result = run_scheduler_inner(&controller) => result,
    };
    if let Err(error) = result {
        let store = Arc::clone(&controller.store);
        let run_id = controller.id.clone();
        let _ = tokio::task::spawn_blocking(move || store.mark_run_failed(&run_id)).await;
        let _ = controller.status.send(RunStatus::Failed(error));
    } else {
        let _ = controller.status.send(RunStatus::Completed);
    }
}

async fn run_scheduler_inner(controller: &Arc<RunController>) -> Result<(), String> {
    let definition = controller.definition.lock().await.clone();
    let total = definition.phases.len();
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
        progress(
            &controller.invocation,
            format!("Running phase {}/{}: {}", index + 1, total, phase.name),
        )
        .await;
        run_phase(controller, index, total).await?;
    }
    if !definition.verification.is_empty() {
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
) -> Result<(), String> {
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
            break Ok(());
        }
        if ready.is_empty() {
            let mut activity = controller.activity_rx.clone();
            activity
                .changed()
                .await
                .map_err(|_| "scheduler activity stopped".to_string())?;
            continue;
        }
        let mut operations = Vec::new();
        for scheduled in ready {
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
            let task_instructions = details.instructions.clone();
            ensure_task_record(
                controller,
                TaskDeclaration {
                    id: details.task_id.clone(),
                    name: details.name.clone(),
                    instructions: details.instructions.clone(),
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
                name: task_def.name.clone(),
                instructions: task_instructions,
                profile: agent_def.profile,
                model: agent_def.model,
                reasoning_effort: agent_def.reasoning_effort.and_then(parse_effort),
            };
            let store = Arc::clone(&controller.store);
            let id = scheduled.task_id.clone();
            tokio::task::spawn_blocking(move || store.mark_task_running(&id))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let task_id = scheduled.task_id.clone();
            operations.push((
                scheduled,
                task_def.clone(),
                run_task_handler(controller.invocation.clone(), task_id, agent),
            ));
        }
        let completed = join_all(
            operations
                .into_iter()
                .map(|(scheduled, task_def, operation)| async move {
                    (scheduled, task_def, operation.await)
                }),
        )
        .await;
        let mut retry_capacity = false;
        let mut first_error = None;
        for (scheduled, task_def, operation) in completed {
            if let Err(error) = operation {
                let store = Arc::clone(&controller.store);
                let task_id = scheduled.task_id.clone();
                if is_capacity_error(&error) {
                    let _ =
                        tokio::task::spawn_blocking(move || store.mark_task_ready(&task_id)).await;
                    retry_capacity = true;
                } else {
                    let _ =
                        tokio::task::spawn_blocking(move || store.mark_task_failed(&task_id)).await;
                    progress(
                        &controller.invocation,
                        format!("Task {} failed during agent", task_def.name),
                    )
                    .await;
                    first_error.get_or_insert_with(|| error.to_string());
                }
                continue;
            }
            if let Err((stage, error)) =
                complete_task(controller, &scheduled.task_id, &task_def).await
            {
                let store = Arc::clone(&controller.store);
                let task_id = scheduled.task_id.clone();
                let _ = tokio::task::spawn_blocking(move || store.mark_task_failed(&task_id)).await;
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
        if retry_capacity {
            continue;
        }
    }
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
) -> Result<(), (String, String)> {
    if !task_def.verification.is_empty() {
        progress(
            &controller.invocation,
            format!("Verifying task: {}", task_def.name),
        )
        .await;
        let verification_output =
            run_verify_handler(controller.invocation.clone(), task_id.to_string())
                .await
                .map_err(|error| ("verification".to_string(), error.to_string()))?;
        if verification_output
            .code_mode_result(&ToolPayload::Function {
                arguments: serde_json::json!({"task_id": task_id}).to_string(),
            })
            .get("passed")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err((
                "verification".to_string(),
                format!("Task {} failed during verification", task_def.name),
            ));
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
    progress(
        &controller.invocation,
        format!("Integrating task: {}", task_def.name),
    )
    .await;
    run_integrate_handler(controller.invocation.clone(), task_id.to_string())
        .await
        .map_err(|error| ("integration".to_string(), error.to_string()))?;
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

fn is_capacity_error(error: &FunctionCallError) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("agent thread limit reached")
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
                "instructions": agent.instructions,
                "profile": agent.profile,
                "model": agent.model,
                "reasoning_effort": agent.reasoning_effort,
            }
        })
        .to_string(),
    };
    FlowdexTaskRunAgentHandler.handle(invocation).await
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
) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
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
