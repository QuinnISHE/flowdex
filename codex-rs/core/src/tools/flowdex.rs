use crate::function_tool::FunctionCallError;
use crate::session::InputQueueActivity;
use crate::tools::code_mode::CodeModeNestedTool;
use crate::tools::code_mode::ExecContext;
use crate::tools::code_mode::execute_source_with_cell_hook;
use crate::tools::code_mode::into_function_call_output_content_items;
use crate::tools::code_mode::truncate_code_mode_result;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_flowdex::FlowdexStore;
use codex_flowdex::PendingBoundary;
use codex_flowdex::PendingSignal;
use codex_flowdex::WorkflowLoader;
use codex_flowdex::WorkflowRef;
use codex_flowdex::WorkflowScope;
use codex_flowdex::save_workflow;
use codex_protocol::models::function_call_output_content_items_to_text;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod agents;
mod context;
mod review;
mod rules;
mod scheduler;
mod task;
mod verification;
pub(crate) use agents::FlowdexResumeAgentHandler;
pub(crate) use agents::FlowdexSendMessageHandler;
pub(crate) use agents::FlowdexSpawnAgentHandler;
pub(crate) use agents::FlowdexWaitAgentHandler;
pub(crate) use context::PublishFlowdexContextHandler;
pub(crate) use context::ReadFlowdexContextHandler;
pub(crate) use review::FlowdexReviewReportHandler;
pub(crate) use review::review_report_tool_visible;
pub(crate) use rules::FlowdexCheckRulesHandler;
pub(crate) use rules::FlowdexScanRuleCandidatesHandler;
pub(crate) use scheduler::{
    FlowdexQueueTaskHandler, FlowdexSealPhaseHandler, FlowdexStartRunHandler,
    FlowdexWaitRunHandler, PauseFlowdexWorkflowHandler, QueueFlowdexTaskHandler,
    ResumeFlowdexWorkflowHandler, SealFlowdexPhaseHandler,
};
pub(crate) use task::FlowdexCreateTaskHandler;
pub(crate) use task::FlowdexTaskIntegrateHandler;
pub(crate) use task::FlowdexTaskRunAgentHandler;
pub(crate) use task::FlowdexTaskVerifyHandler;
pub(crate) use verification::FlowdexExecCommandHandler;
pub(crate) use verification::FlowdexVerifyHandler;

const TOOL_NAME: &str = "start_flowdex_workflow";
const WAIT_TOOL_NAME: &str = "wait_flowdex_workflow";
const RUN_TOOL_NAME: &str = "flowdex_run_workflow";
const SAVE_TOOL_NAME: &str = "save_flowdex_workflow";
const CONTINUE_TOOL_NAME: &str = "continue_flowdex_workflow";
const SIGNAL_TOOL_NAME: &str = "flowdex_signal";

struct BoundaryState {
    store: Arc<FlowdexStore>,
    signal: watch::Sender<Option<PendingBoundary>>,
    activity: watch::Sender<u64>,
    activity_generation: u64,
    terminal: bool,
    consumed: bool,
}

static BOUNDARIES: OnceLock<Mutex<HashMap<String, BoundaryState>>> = OnceLock::new();

fn boundaries() -> &'static Mutex<HashMap<String, BoundaryState>> {
    BOUNDARIES.get_or_init(Default::default)
}

/// Registers the event channel used by a live scheduler run.
pub(crate) fn register_flowdex_boundary_run(run_id: impl Into<String>, store: Arc<FlowdexStore>) {
    let (signal, _) = watch::channel(None);
    let (activity, _) = watch::channel(0);
    boundaries()
        .lock()
        .expect("Flowdex boundary registry poisoned")
        .insert(
            run_id.into(),
            BoundaryState {
                store,
                signal,
                activity,
                activity_generation: 0,
                terminal: false,
                consumed: false,
            },
        );
}

pub(crate) async fn publish_flowdex_boundary(
    store: Arc<FlowdexStore>,
    boundary: PendingBoundary,
) -> Result<(), String> {
    let persisted = boundary.clone();
    let store_for_persist = Arc::clone(&store);
    tokio::task::spawn_blocking(move || store_for_persist.set_pending_boundary(&persisted))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let mut registry = boundaries()
        .lock()
        .map_err(|_| "Flowdex boundary registry poisoned".to_string())?;
    let state = registry.entry(boundary.run_id.clone()).or_insert_with(|| {
        let (signal, _) = watch::channel(None);
        let (activity, _) = watch::channel(0);
        BoundaryState {
            store: Arc::clone(&store),
            signal,
            activity,
            activity_generation: 0,
            terminal: false,
            consumed: false,
        }
    });
    state.consumed = false;
    state.activity_generation = state.activity_generation.wrapping_add(1);
    let _ = state.activity.send(state.activity_generation);
    state.signal.send_replace(Some(boundary));
    Ok(())
}

pub(crate) async fn publish_flowdex_signal(
    store: Arc<FlowdexStore>,
    run_id: String,
    signal: String,
) -> Result<(), String> {
    let store_for_persist = Arc::clone(&store);
    let run_id_for_persist = run_id.clone();
    tokio::task::spawn_blocking(move || {
        store_for_persist.enqueue_signal(&run_id_for_persist, &signal)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    let mut registry = boundaries()
        .lock()
        .map_err(|_| "Flowdex boundary registry poisoned".to_string())?;
    let state = registry.entry(run_id).or_insert_with(|| {
        let (signal_tx, _) = watch::channel(None);
        let (activity, _) = watch::channel(0);
        BoundaryState {
            store: Arc::clone(&store),
            signal: signal_tx,
            activity,
            activity_generation: 0,
            terminal: false,
            consumed: false,
        }
    });
    state.activity_generation = state.activity_generation.wrapping_add(1);
    state
        .activity
        .send(state.activity_generation)
        .map_err(|_| "Flowdex signal waiters stopped".to_string())
}

async fn signal_waiter(run_id: String) -> PendingSignal {
    let (store, mut activity) = match boundaries().lock().ok().and_then(|registry| {
        registry
            .get(&run_id)
            .map(|state| (Arc::clone(&state.store), state.activity.subscribe()))
    }) {
        Some(value) => value,
        None => return std::future::pending().await,
    };
    loop {
        let pending = tokio::task::spawn_blocking({
            let store = Arc::clone(&store);
            let run_id = run_id.clone();
            move || store.oldest_pending_signal(&run_id)
        })
        .await;
        if let Ok(Ok(Some(signal))) = pending {
            return signal;
        }
        if activity.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

async fn signal_store(run_id: &str) -> Option<Arc<FlowdexStore>> {
    boundaries()
        .lock()
        .ok()
        .and_then(|registry| registry.get(run_id).map(|state| Arc::clone(&state.store)))
}

pub(crate) fn mark_flowdex_boundary_terminal(run_id: &str) {
    if let Ok(mut registry) = boundaries().lock() {
        if let Some(state) = registry.get_mut(run_id) {
            state.terminal = true;
        }
    }
}

pub(crate) async fn subscribe_flowdex_boundary(
    run_id: &str,
) -> Option<watch::Receiver<Option<PendingBoundary>>> {
    boundaries()
        .lock()
        .ok()
        .and_then(|registry| registry.get(run_id).map(|state| state.signal.subscribe()))
}

#[derive(Clone, Debug, Default)]
struct WorkflowInvocation {
    chain: Vec<String>,
    parent_run_id: Option<String>,
    workflow_identity: Option<String>,
}

static WORKFLOW_CHAINS: OnceLock<Mutex<HashMap<String, WorkflowInvocation>>> = OnceLock::new();
static WORKFLOW_RUN_IDS: OnceLock<Mutex<WorkflowRunIds>> = OnceLock::new();

#[derive(Default)]
struct WorkflowRunIds {
    by_cell: HashMap<String, String>,
    by_run: HashMap<String, String>,
}

fn workflow_chains() -> &'static Mutex<HashMap<String, WorkflowInvocation>> {
    WORKFLOW_CHAINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn workflow_run_ids() -> &'static Mutex<WorkflowRunIds> {
    WORKFLOW_RUN_IDS.get_or_init(|| Mutex::new(WorkflowRunIds::default()))
}

fn register_workflow_run(cell_id: &codex_code_mode::CellId) -> String {
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut ids = workflow_run_ids()
        .lock()
        .expect("workflow run id mutex poisoned");
    ids.by_cell.insert(cell_id.to_string(), run_id.clone());
    ids.by_run.insert(run_id.clone(), cell_id.to_string());
    run_id
}

pub(super) fn workflow_store_run_id(cell_id: &str) -> String {
    workflow_run_ids()
        .lock()
        .expect("workflow run id mutex poisoned")
        .by_cell
        .get(cell_id)
        .cloned()
        .unwrap_or_else(|| cell_id.to_string())
}

fn workflow_cell_id(run_id: &str) -> codex_code_mode::CellId {
    let cell_id = workflow_run_ids()
        .lock()
        .expect("workflow run id mutex poisoned")
        .by_run
        .get(run_id)
        .cloned()
        .unwrap_or_else(|| run_id.to_string());
    codex_code_mode::CellId::new(cell_id)
}

fn remove_workflow_run(cell_id: &str) {
    let mut ids = workflow_run_ids()
        .lock()
        .expect("workflow run id mutex poisoned");
    if let Some(run_id) = ids.by_cell.remove(cell_id) {
        ids.by_run.remove(&run_id);
    }
}

pub(super) fn workflow_run_identity(cell_id: &str) -> (Option<String>, Option<String>) {
    let cell_id = workflow_cell_id(cell_id);
    workflow_chains()
        .lock()
        .expect("workflow chain mutex poisoned")
        .get(cell_id.as_str())
        .map(|invocation| {
            (
                invocation.parent_run_id.clone(),
                invocation.workflow_identity.clone(),
            )
        })
        .unwrap_or_default()
}

fn child_workflow_chain(parent_cell: &str, identity: &str) -> Result<Vec<String>, String> {
    let chains = workflow_chains()
        .lock()
        .expect("workflow chain mutex poisoned");
    let mut chain = chains
        .get(parent_cell)
        .map(|invocation| invocation.chain.clone())
        .unwrap_or_default();
    if chain.iter().any(|item| item == identity) {
        return Err("Flowdex workflow reference cycle detected".to_string());
    }
    chain.push(identity.to_string());
    Ok(chain)
}

#[derive(Debug, Deserialize)]
struct StartArgs {
    path: String,
    #[serde(default)]
    input: Option<Value>,
}

pub(crate) struct StartFlowdexWorkflowHandler {
    nested_tool_specs: Vec<CodeModeNestedTool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunWorkflowArgs {
    workflow: String,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveWorkflowArgs {
    workflow: String,
    source: String,
}

pub(crate) struct FlowdexRunWorkflowHandler {
    nested_tool_specs: Vec<CodeModeNestedTool>,
}

pub(crate) struct SaveFlowdexWorkflowHandler;

pub(crate) struct WaitFlowdexWorkflowHandler;
pub(crate) struct ContinueFlowdexWorkflowHandler;
pub(crate) struct FlowdexSignalHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalArgs {
    signal: String,
}

impl ToolExecutor<ToolInvocation> for FlowdexSignalHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SIGNAL_TOOL_NAME)
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: SIGNAL_TOOL_NAME.to_string(),
            description: "Publish a named signal to a saved Flowdex workflow.".to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([("signal".to_string(), JsonSchema::string(None))]),
                Some(vec!["signal".to_string()]),
                Some(false.into()),
            ),
            output_schema: Some(serde_json::json!({"type":"null"})),
        })
    }
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolInvocation {
                payload: ToolPayload::Function { arguments },
                source,
                ..
            } = invocation
            else {
                return Err(FunctionCallError::RespondToModel(
                    "flowdex.signal expects JSON arguments".into(),
                ));
            };
            let args: SignalArgs = parse_arguments(&arguments)?;
            if args.signal.trim().is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "signal name must be non-empty".into(),
                ));
            }
            let crate::tools::context::ToolCallSource::CodeMode { cell_id, .. } = source else {
                return Err(FunctionCallError::RespondToModel(
                    "flowdex.signal is available only inside saved workflows".into(),
                ));
            };
            let run_id = workflow_store_run_id(&cell_id);
            let Some(store) = signal_store(&run_id).await else {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex workflow is not active".into(),
                ));
            };
            publish_flowdex_signal(store, run_id, args.signal)
                .await
                .map_err(FunctionCallError::RespondToModel)?;
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                "null".to_string(),
                Some(true),
            )))
        })
    }
}
impl CoreToolRuntime for FlowdexSignalHandler {}

impl ToolExecutor<ToolInvocation> for ContinueFlowdexWorkflowHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(CONTINUE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: CONTINUE_TOOL_NAME.to_string(),
            description: "Continue the current durable Flowdex boundary once.".to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([("run_id".to_string(), JsonSchema::string(None))]),
                Some(vec!["run_id".to_string()]),
                Some(false.into()),
            ),
            output_schema: Some(serde_json::json!({"type": "object"})),
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move { self.handle_call(invocation).await })
    }
}

impl CoreToolRuntime for ContinueFlowdexWorkflowHandler {}

impl ContinueFlowdexWorkflowHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            payload: ToolPayload::Function { arguments },
            ..
        } = invocation
        else {
            return Err(FunctionCallError::RespondToModel(
                "continue_flowdex_workflow expects JSON arguments".into(),
            ));
        };
        let args: WaitArgs = parse_arguments(&arguments)?;
        let (store, boundary) = {
            let registry = boundaries().lock().map_err(|_| {
                FunctionCallError::RespondToModel("Flowdex boundary registry unavailable".into())
            })?;
            let state = registry.get(&args.run_id).ok_or_else(|| {
                FunctionCallError::RespondToModel("Flowdex boundary not found".into())
            })?;
            if state.terminal {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex run is terminal".into(),
                ));
            }
            if state.consumed || state.signal.borrow().is_none() {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex boundary is stale or already consumed".into(),
                ));
            }
            let boundary = state.signal.borrow().clone();
            (Arc::clone(&state.store), boundary)
        };
        let Some(boundary) = boundary else {
            return Err(FunctionCallError::RespondToModel(
                "Flowdex boundary is stale or already consumed".into(),
            ));
        };
        let run_id = args.run_id.clone();
        let scope_kind = boundary.scope_kind.clone();
        let scope_id = boundary.scope_id.clone();
        tokio::task::spawn_blocking(move || {
            store.clear_pending_boundary(&run_id, &scope_kind, &scope_id)
        })
        .await
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        let mut registry = boundaries().lock().map_err(|_| {
            FunctionCallError::RespondToModel("Flowdex boundary registry unavailable".into())
        })?;
        let state = registry.get_mut(&args.run_id).ok_or_else(|| {
            FunctionCallError::RespondToModel("Flowdex boundary not found".into())
        })?;
        if state.terminal || state.consumed || state.signal.borrow().as_ref() != Some(&boundary) {
            return Err(FunctionCallError::RespondToModel(
                "Flowdex boundary is stale or already consumed".into(),
            ));
        }
        state.consumed = true;
        state.signal.send(None).map_err(|_| {
            FunctionCallError::RespondToModel("Flowdex boundary waiters stopped".into())
        })?;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            serde_json::json!({"runId": args.run_id, "status": "continued"}).to_string(),
            Some(true),
        )))
    }
}

impl ToolExecutor<ToolInvocation> for WaitFlowdexWorkflowHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(WAIT_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: WAIT_TOOL_NAME.to_string(),
            description: "Wait for a Flowdex workflow lifecycle event or turn input.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([("run_id".to_string(), JsonSchema::string(None))]),
                Some(vec!["run_id".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation))
    }
}

impl CoreToolRuntime for WaitFlowdexWorkflowHandler {}

impl WaitFlowdexWorkflowHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            cancellation_token,
            payload: ToolPayload::Function { arguments },
            ..
        } = invocation
        else {
            return Err(FunctionCallError::RespondToModel(
                "wait_flowdex_workflow expects JSON arguments".to_string(),
            ));
        };
        let args: WaitArgs = parse_arguments(&arguments)?;
        let cell_id = workflow_cell_id(&args.run_id);
        let turn_state = session
            .input_queue
            .turn_state_for_sub_id(&session.active_turn, &turn.sub_id)
            .await;
        let (mut activity_rx, pending_activity) = session
            .input_queue
            .subscribe_activity(turn_state.as_deref())
            .await;
        if matches!(pending_activity, Some(InputQueueActivity::Steer)) {
            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                serde_json::json!({"runId": args.run_id, "status": "steered"}).to_string(),
                Some(true),
            )));
        }

        let mut boundary_wait = Box::pin(wait_for_flowdex_boundary(args.run_id.clone()));
        let mut signal_wait = Box::pin(signal_waiter(args.run_id.clone()));
        let detached_resume = scheduler::is_detached_resume(&args.run_id).await;
        let mut scheduler_wait = Box::pin(scheduler::wait_for_run_event(args.run_id.clone()));
        if matches!(pending_activity, Some(InputQueueActivity::Mailbox))
            && session.input_queue.has_trigger_turn_mailbox_items().await
        {
            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                serde_json::json!({"runId": args.run_id, "status": "message"}).to_string(),
                Some(true),
            )));
        }

        let mut cell_wait = Box::pin(async {
            if detached_resume {
                std::future::pending().await
            } else {
                session
                    .services
                    .code_mode_service
                    .wait_until_yield(cell_id.clone())
                    .await
            }
        });
        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    return Err(FunctionCallError::RespondToModel("wait_flowdex_workflow cancelled".to_string()));
                }
                result = &mut cell_wait => {
                    let wait_response = result.map_err(FunctionCallError::RespondToModel)?;
                    if let codex_code_mode::WaitOutcome::LiveCell(response) = &wait_response
                        && !matches!(response, codex_code_mode::RuntimeResponse::Yielded { .. })
                    {
                        let runtime_cell_id = match response {
                            codex_code_mode::RuntimeResponse::Yielded { cell_id, .. }
                            | codex_code_mode::RuntimeResponse::Terminated { cell_id, .. }
                            | codex_code_mode::RuntimeResponse::Result { cell_id, .. } => cell_id,
                        };
                        session
                            .services
                            .rollout_thread_trace
                            .code_cell_trace_context(&turn.sub_id, runtime_cell_id.as_str())
                            .record_ended(&response);
                        session
                            .services
                            .code_mode_service
                            .finish_cell_dispatch(&runtime_cell_id);
                        workflow_chains()
                            .lock()
                            .expect("workflow chain mutex poisoned")
                            .remove(runtime_cell_id.as_str());
                        remove_workflow_run(runtime_cell_id.as_str());
                    }
                    let (result, terminal) = flowdex_result(args.run_id.clone(), wait_response.into());
                    if terminal {
                        session.services.elicitations.wait_until_clear().await;
                    }
                    return Ok(boxed_tool_output(FunctionToolOutput::from_text(result.to_string(), Some(true))));
                }
                event = &mut scheduler_wait => {
                    match event {
                        scheduler::SchedulerEvent::Paused => {
                            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                                serde_json::json!({"runId": args.run_id, "status": "paused"}).to_string(),
                                Some(true),
                            )));
                        }
                        scheduler::SchedulerEvent::Completed => {
                            session.services.elicitations.wait_until_clear().await;
                            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                                serde_json::json!({"runId": args.run_id, "status": "completed"}).to_string(),
                                Some(true),
                            )));
                        }
                        scheduler::SchedulerEvent::Failed(error) => {
                            if !detached_resume {
                                if let Ok(codex_code_mode::WaitOutcome::LiveCell(response)) = session
                                    .services
                                    .code_mode_service
                                    .terminate(cell_id.clone())
                                    .await
                                {
                                    session
                                        .services
                                        .rollout_thread_trace
                                        .code_cell_trace_context(&turn.sub_id, cell_id.as_str())
                                        .record_ended(&response);
                                }
                                session.services.code_mode_service.finish_cell_dispatch(&cell_id);
                                workflow_chains()
                                    .lock()
                                    .expect("workflow chain mutex poisoned")
                                    .remove(cell_id.as_str());
                                remove_workflow_run(cell_id.as_str());
                            }
                            session.services.elicitations.wait_until_clear().await;
                            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                                serde_json::json!({
                                    "runId": args.run_id,
                                    "status": "failed",
                                    "error": agents::truncate_message(&error),
                                })
                                .to_string(),
                                Some(false),
                            )));
                        }
                    }
                }
                pending = &mut signal_wait => {
                    let pending_steer = match turn_state.as_deref() {
                        Some(turn_state) => session.input_queue.has_pending_user_input_for_turn_state(turn_state).await,
                        None => false,
                    };
                    if pending_steer {
                        return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                            serde_json::json!({"runId": args.run_id, "status": "steered"}).to_string(), Some(true))));
                    }
                    let store = signal_store(&args.run_id).await.ok_or_else(|| FunctionCallError::RespondToModel("Flowdex workflow is not active".into()))?;
                    let signal_id = pending.id;
                    let run_id = args.run_id.clone();
                    tokio::task::spawn_blocking(move || store.consume_signal(&run_id, signal_id))
                        .await.map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
                        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
                    let pending_steer = match turn_state.as_deref() {
                        Some(turn_state) => session.input_queue.has_pending_user_input_for_turn_state(turn_state).await,
                        None => false,
                    };
                    if pending_steer {
                        let store = signal_store(&args.run_id).await.ok_or_else(|| FunctionCallError::RespondToModel("Flowdex workflow is not active".into()))?;
                        let signal_id = pending.id;
                        let run_id = args.run_id.clone();
                        let signal = pending.signal.clone();
                        tokio::task::spawn_blocking(move || store.restore_signal(&run_id, signal_id, &signal))
                            .await.map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
                            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
                        return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                            serde_json::json!({"runId": args.run_id, "status": "steered"}).to_string(), Some(true))));
                    }
                    return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                        serde_json::json!({"runId": args.run_id, "status": "signal", "signal": pending.signal}).to_string(), Some(true))));
                }
                boundary = &mut boundary_wait => {
                    return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                        flowdex_boundary_result(boundary).to_string(),
                        Some(true),
                    )));
                }
                changed = activity_rx.changed() => {
                    if changed.is_err() {
                        return Err(FunctionCallError::RespondToModel("input queue activity stopped".to_string()));
                    }
                    let pending_steer = match turn_state.as_deref() {
                        Some(turn_state) => session
                            .input_queue
                            .has_pending_user_input_for_turn_state(turn_state)
                            .await,
                        None => false,
                    };
                    if pending_steer {
                        return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                            serde_json::json!({"runId": args.run_id, "status": "steered"}).to_string(),
                            Some(true),
                        )));
                    }
                    let activity = *activity_rx.borrow_and_update();
                    match activity {
                        InputQueueActivity::Steer => {
                            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                                serde_json::json!({"runId": args.run_id, "status": "steered"}).to_string(),
                                Some(true),
                            )));
                        }
                        InputQueueActivity::Mailbox if session.input_queue.has_trigger_turn_mailbox_items().await => {
                            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                                serde_json::json!({"runId": args.run_id, "status": "message"}).to_string(),
                                Some(true),
                            )));
                        }
                        InputQueueActivity::Mailbox => continue,
                    }
                }
            }
        }
    }
}

pub(crate) async fn wait_for_flowdex_boundary(run_id: String) -> PendingBoundary {
    let Some(mut signal) = subscribe_flowdex_boundary(&run_id).await else {
        return std::future::pending().await;
    };
    loop {
        if let Some(boundary) = signal.borrow_and_update().clone() {
            return boundary;
        }
        if signal.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

pub(crate) async fn wait_flowdex_boundary_continuation(
    run_id: &str,
    scope_kind: &str,
    scope_id: &str,
) {
    let Some(mut signal) = subscribe_flowdex_boundary(run_id).await else {
        return;
    };
    loop {
        let matching = signal.borrow().as_ref().is_some_and(|boundary| {
            boundary.scope_kind == scope_kind && boundary.scope_id == scope_id
        });
        if signal.changed().await.is_err() {
            return;
        }
        if matching && signal.borrow().is_none() {
            return;
        }
    }
}

fn flowdex_boundary_result(boundary: PendingBoundary) -> Value {
    serde_json::json!({
        "runId": boundary.run_id,
        "status": "boundary",
        "scope": {"kind": boundary.scope_kind, "name": boundary.scope_id},
        "target": boundary.target,
        "reason": boundary.reason,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    run_id: String,
}

impl StartFlowdexWorkflowHandler {
    pub(crate) fn new(nested_tool_specs: Vec<CodeModeNestedTool>) -> Self {
        Self { nested_tool_specs }
    }
}

impl FlowdexRunWorkflowHandler {
    pub(crate) fn new(nested_tool_specs: Vec<CodeModeNestedTool>) -> Self {
        Self { nested_tool_specs }
    }
}

impl ToolExecutor<ToolInvocation> for FlowdexRunWorkflowHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(RUN_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: RUN_TOOL_NAME.to_string(),
            description: "Run a saved Flowdex workflow without a model turn.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    ("workflow".to_string(), JsonSchema::string(None)),
                    ("input".to_string(), JsonSchema::default()),
                ]),
                Some(vec!["workflow".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation))
    }
}

impl CoreToolRuntime for FlowdexRunWorkflowHandler {}

impl FlowdexRunWorkflowHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let originating_item_id = invocation.originating_item_id().await;
        let ToolInvocation {
            session,
            turn,
            call_id,
            source,
            cancellation_token,
            payload: ToolPayload::Function { arguments },
            ..
        } = invocation
        else {
            return Err(FunctionCallError::RespondToModel(
                "flowdex.runWorkflow expects JSON arguments".to_string(),
            ));
        };
        let args: RunWorkflowArgs = parse_arguments(&arguments)?;
        let reference = WorkflowRef::parse(&args.workflow)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let parent_cell = match source {
            crate::tools::context::ToolCallSource::CodeMode { cell_id, .. } => cell_id,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "flowdex.runWorkflow is available only inside a workflow".to_string(),
                ));
            }
        };
        let identity = reference.normalized_display();
        let child_chain = child_workflow_chain(&parent_cell, &identity)
            .map_err(FunctionCallError::RespondToModel)?;
        let root = match reference.scope() {
            WorkflowScope::Repo => {
                if !turn.config.active_project.is_trusted() {
                    return Err(FunctionCallError::RespondToModel(
                        "Flowdex workflows require a trusted Git repository".to_string(),
                    ));
                }
                turn.environments
                    .single_local_environment_cwd()
                    .ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "flowdex.runWorkflow requires one local environment".to_string(),
                        )
                    })?
                    .to_path_buf()
            }
            WorkflowScope::Global => turn.config.codex_home.to_path_buf(),
        };
        let loader_root = AbsolutePathBuf::from_absolute_path(root.clone())
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let loaded = WorkflowLoader::new(loader_root)
            .load_reference(&reference, &root, args.input.as_ref())
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let exec = ExecContext { session, turn };
        let chain_for_registration = child_chain.clone();
        let identity_for_registration = identity.clone();
        let parent_run_id = workflow_store_run_id(&parent_cell);
        let inherited_store = signal_store(&parent_run_id).await;
        let started_child = Arc::new(Mutex::new(None));
        let started_child_for_registration = Arc::clone(&started_child);
        let (response, child_cell, _) = tokio::select! {
            _ = cancellation_token.cancelled() => {
                let child_cell = started_child
                    .lock()
                    .expect("child cell mutex poisoned")
                    .clone();
                if let Some(child_cell) = child_cell {
                    let terminal = exec
                        .session
                        .services
                        .code_mode_service
                        .terminate(child_cell.clone())
                        .await
                        .ok()
                        .map(Into::into);
                    finish_nested_dispatch(&exec, &child_cell, terminal.as_ref());
                }
                return Err(FunctionCallError::RespondToModel("Flowdex nested workflow cancelled".to_string()));
            }
            result = execute_source_with_cell_hook(
                &exec,
                format!("{RUN_TOOL_NAME}-{call_id}"),
                loaded.into_source(),
                &self.nested_tool_specs,
                None,
                None,
                originating_item_id,
                Some(Box::new(move |child_cell| {
                    let child_run_id = register_workflow_run(child_cell);
                    *started_child_for_registration
                        .lock()
                        .expect("child cell mutex poisoned") = Some(child_cell.clone());
                    workflow_chains()
                        .lock()
                        .expect("workflow chain mutex poisoned")
                        .insert(
                            child_cell.to_string(),
                            WorkflowInvocation {
                                chain: chain_for_registration,
                                parent_run_id: Some(parent_run_id),
                                workflow_identity: Some(identity_for_registration),
                            },
                        );
                    if let Some(store) = inherited_store {
                        register_flowdex_boundary_run(child_run_id, store);
                    }
                })),
            ) => result.map_err(FunctionCallError::RespondToModel)?,
        };
        let result =
            wait_nested_workflow(&exec, child_cell.clone(), response, cancellation_token).await;
        workflow_chains()
            .lock()
            .expect("workflow chain mutex poisoned")
            .remove(child_cell.as_str());
        remove_workflow_run(child_cell.as_str());
        let value = result.map_err(FunctionCallError::RespondToModel)?;
        Ok(boxed_tool_output(task::JsonOutput(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_workflow_chain_rejects_recursive_reference() {
        let parent = "flowdex-cycle-test-parent";
        workflow_chains()
            .lock()
            .expect("workflow chain mutex poisoned")
            .insert(
                parent.to_string(),
                WorkflowInvocation {
                    chain: vec!["repo:cycle".to_string()],
                    ..Default::default()
                },
            );
        assert_eq!(
            child_workflow_chain(parent, "repo:cycle"),
            Err("Flowdex workflow reference cycle detected".to_string())
        );
        workflow_chains()
            .lock()
            .expect("workflow chain mutex poisoned")
            .remove(parent);
    }
}

async fn wait_nested_workflow(
    exec: &ExecContext,
    cell_id: codex_code_mode::CellId,
    mut response: codex_code_mode::RuntimeResponse,
    cancellation_token: CancellationToken,
) -> Result<Value, String> {
    loop {
        if matches!(
            response,
            codex_code_mode::RuntimeResponse::Result { .. }
                | codex_code_mode::RuntimeResponse::Terminated { .. }
        ) {
            let terminal = response.clone();
            exec.session
                .services
                .rollout_thread_trace
                .code_cell_trace_context(&exec.turn.sub_id, cell_id.as_str())
                .record_ended(&terminal);
            exec.session
                .services
                .code_mode_service
                .finish_cell_dispatch(&cell_id);
            return nested_workflow_result(terminal);
        }
        let wait = tokio::select! {
            _ = cancellation_token.cancelled() => {
                if let Ok(terminal) = exec
                    .session
                    .services
                    .code_mode_service
                    .terminate(cell_id.clone())
                    .await
                {
                    let terminal: codex_code_mode::RuntimeResponse = terminal.into();
                    finish_nested_dispatch(exec, &cell_id, Some(&terminal));
                } else {
                    finish_nested_dispatch(exec, &cell_id, Some(&response));
                }
                return Err("Flowdex nested workflow cancelled".to_string());
            }
            wait = exec.session.services.code_mode_service.wait(codex_code_mode::WaitRequest { cell_id: cell_id.clone(), yield_time_ms: 10_000 }) => match wait {
                Ok(wait) => wait,
                Err(error) => {
                    let terminal = exec
                        .session
                        .services
                        .code_mode_service
                        .terminate(cell_id.clone())
                        .await
                        .ok()
                        .map(Into::into);
                    finish_nested_dispatch(exec, &cell_id, terminal.as_ref().or(Some(&response)));
                    return Err(error);
                }
            },
        };
        response = wait.into();
    }
}

fn finish_nested_dispatch(
    exec: &ExecContext,
    cell_id: &codex_code_mode::CellId,
    terminal: Option<&codex_code_mode::RuntimeResponse>,
) {
    if let Some(terminal) = terminal {
        exec.session
            .services
            .rollout_thread_trace
            .code_cell_trace_context(&exec.turn.sub_id, cell_id.as_str())
            .record_ended(terminal);
    }
    exec.session
        .services
        .code_mode_service
        .finish_cell_dispatch(cell_id);
}

fn nested_workflow_result(response: codex_code_mode::RuntimeResponse) -> Result<Value, String> {
    let codex_code_mode::RuntimeResponse::Result {
        content_items,
        error_text,
        ..
    } = response
    else {
        return Err("Flowdex nested workflow terminated".to_string());
    };
    if let Some(error) = error_text {
        let bounded = truncate_code_mode_result(
            vec![codex_protocol::models::FunctionCallOutputContentItem::InputText { text: error }],
            None,
        );
        let error = function_call_output_content_items_to_text(&bounded).unwrap_or_default();
        return Err(format!("Flowdex nested workflow failed: {error}"));
    }
    let items = into_function_call_output_content_items(content_items);
    let output = function_call_output_content_items_to_text(&items)
        .ok_or_else(|| "Flowdex nested workflow output is invalid".to_string())?;
    if output.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&output)
        .map_err(|_| "Flowdex nested workflow output must be JSON".to_string())
}

impl ToolExecutor<ToolInvocation> for SaveFlowdexWorkflowHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SAVE_TOOL_NAME)
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: SAVE_TOOL_NAME.to_string(),
            description: "Save a named Flowdex workflow source.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    (String::from("workflow"), JsonSchema::string(None)),
                    (String::from("source"), JsonSchema::string(None)),
                ]),
                Some(vec!["workflow".to_string(), "source".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation))
    }
}
impl CoreToolRuntime for SaveFlowdexWorkflowHandler {}
impl SaveFlowdexWorkflowHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            turn,
            payload: ToolPayload::Function { arguments },
            ..
        } = invocation
        else {
            return Err(FunctionCallError::RespondToModel(
                "save_flowdex_workflow expects JSON arguments".into(),
            ));
        };
        let args: SaveWorkflowArgs = parse_arguments(&arguments)?;
        let reference = WorkflowRef::parse(&args.workflow)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        if matches!(reference.scope(), WorkflowScope::Repo)
            && !turn.config.active_project.is_trusted()
        {
            return Err(FunctionCallError::RespondToModel(
                "Flowdex workflows require a trusted Git repository".into(),
            ));
        }
        let root = match reference.scope() {
            WorkflowScope::Repo => turn
                .environments
                .single_local_environment_cwd()
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "save_flowdex_workflow requires one local environment".into(),
                    )
                })?
                .to_path_buf(),
            WorkflowScope::Global => turn.config.codex_home.to_path_buf(),
        };
        let saved =
            tokio::task::spawn_blocking(move || save_workflow(&reference, &root, &args.source))
                .await
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            serde_json::json!({"workflow": saved.normalized_display()}).to_string(),
            Some(true),
        )))
    }
}

impl ToolExecutor<ToolInvocation> for StartFlowdexWorkflowHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let properties = std::collections::BTreeMap::from([
            ("path".to_string(), JsonSchema::string(None)),
            ("input".to_string(), JsonSchema::default()),
        ]);
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: "Start a saved Flowdex workflow from .flowdex/workflows.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["path".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation))
    }
}

impl StartFlowdexWorkflowHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let originating_item_id = invocation.originating_item_id().await;
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload: ToolPayload::Function { arguments },
            ..
        } = invocation
        else {
            return Err(FunctionCallError::RespondToModel(
                "start_flowdex_workflow expects JSON arguments".to_string(),
            ));
        };
        let args: StartArgs = parse_arguments(&arguments)?;
        let reference = WorkflowRef::parse(&args.path).ok();
        let cwd = match reference.as_ref().map(WorkflowRef::scope) {
            Some(WorkflowScope::Global) => None,
            _ => Some(
                turn.environments
                    .single_local_environment_cwd()
                    .ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "start_flowdex_workflow requires one local environment".to_string(),
                        )
                    })?,
            ),
        };
        let loaded = if let Some(reference) = reference.clone() {
            if matches!(reference.scope(), WorkflowScope::Repo)
                && !turn.config.active_project.is_trusted()
            {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex workflows require a trusted Git repository".to_string(),
                ));
            }
            let root = match reference.scope() {
                WorkflowScope::Repo => cwd
                    .as_ref()
                    .expect("repo workflows require cwd")
                    .to_path_buf(),
                WorkflowScope::Global => turn.config.codex_home.to_path_buf(),
            };
            let loader_root = AbsolutePathBuf::from_absolute_path(root.clone())
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            WorkflowLoader::new(loader_root)
                .load_reference(&reference, &root, args.input.as_ref())
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
        } else {
            WorkflowLoader::new(cwd.expect("path workflows require cwd"))
                .load(Path::new(&args.path), args.input.as_ref())
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
        };
        let exec = ExecContext { session, turn };
        let root_store = match reference.as_ref().map(WorkflowRef::scope) {
            Some(WorkflowScope::Repo) => Some(Arc::new(
                task::open_store(&exec.session, &exec.turn).await?.0,
            )),
            Some(WorkflowScope::Global)
                if exec.turn.config.active_project.is_trusted()
                    && exec
                        .turn
                        .environments
                        .single_local_environment_cwd()
                        .is_some() =>
            {
                task::open_store(&exec.session, &exec.turn)
                    .await
                    .ok()
                    .map(|(store, _, _)| Arc::new(store))
            }
            _ => None,
        };
        let workflow_metadata = reference.as_ref().map(|reference| {
            (
                reference.normalized_display(),
                reference.normalized_display(),
            )
        });
        let run_id = Arc::new(Mutex::new(None));
        let run_id_for_registration = Arc::clone(&run_id);
        let (response, cell_id, _started_at) = execute_source_with_cell_hook(
            &exec,
            call_id,
            loaded.into_source(),
            &self.nested_tool_specs,
            None,
            None,
            originating_item_id,
            Some(Box::new(move |cell_id: &codex_code_mode::CellId| {
                let public_run_id = register_workflow_run(cell_id);
                *run_id_for_registration
                    .lock()
                    .expect("workflow run id result mutex poisoned") = Some(public_run_id.clone());
                if let Some((identity, chain_identity)) = workflow_metadata {
                    workflow_chains()
                        .lock()
                        .expect("workflow chain mutex poisoned")
                        .insert(
                            cell_id.to_string(),
                            WorkflowInvocation {
                                chain: vec![chain_identity],
                                parent_run_id: None,
                                workflow_identity: Some(identity),
                            },
                        );
                }
                if let Some(store) = root_store {
                    register_flowdex_boundary_run(public_run_id, store);
                }
            })),
        )
        .await
        .map_err(FunctionCallError::RespondToModel)?;
        if matches!(
            response,
            codex_code_mode::RuntimeResponse::Result { .. }
                | codex_code_mode::RuntimeResponse::Terminated { .. }
        ) {
            workflow_chains()
                .lock()
                .expect("workflow chain mutex poisoned")
                .remove(cell_id.as_str());
            remove_workflow_run(cell_id.as_str());
        }
        let run_id = run_id
            .lock()
            .expect("workflow run id result mutex poisoned")
            .clone()
            .expect("workflow run id should be registered before execution");
        let (result, _) = flowdex_result(run_id, response);
        let success = result.get("status").and_then(Value::as_str) != Some("failed");
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            result.to_string(),
            Some(success),
        )))
    }
}

fn flowdex_result(run_id: String, response: codex_code_mode::RuntimeResponse) -> (Value, bool) {
    let (status, content_items, error_text, terminal) = match response {
        codex_code_mode::RuntimeResponse::Yielded { content_items, .. } => {
            ("yielded", content_items, None, false)
        }
        codex_code_mode::RuntimeResponse::Terminated { content_items, .. } => {
            ("terminated", content_items, None, true)
        }
        codex_code_mode::RuntimeResponse::Result {
            content_items,
            error_text,
            ..
        } => ("completed", content_items, error_text, true),
    };
    let content_items =
        truncate_code_mode_result(into_function_call_output_content_items(content_items), None);
    let output = function_call_output_content_items_to_text(&content_items).unwrap_or_default();
    let error = error_text.map(|error| {
        let items = truncate_code_mode_result(
            vec![codex_protocol::models::FunctionCallOutputContentItem::InputText { text: error }],
            None,
        );
        function_call_output_content_items_to_text(&items).unwrap_or_default()
    });
    let mut result = serde_json::json!({
        "runId": run_id,
        "status": if error.is_some() { "failed" } else { status },
        "output": output,
    });
    if let Some(error) = error {
        result["error"] = Value::String(error);
    }
    (result, terminal)
}

impl CoreToolRuntime for StartFlowdexWorkflowHandler {}
