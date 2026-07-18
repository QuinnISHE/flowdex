use crate::function_tool::FunctionCallError;
use crate::session::InputQueueActivity;
use crate::tools::code_mode::ExecContext;
use crate::tools::code_mode::execute_source;
use crate::tools::code_mode::into_function_call_output_content_items;
use crate::tools::code_mode::truncate_code_mode_result;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_flowdex::WorkflowLoader;
use codex_protocol::models::function_call_output_content_items_to_text;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

mod agents;
mod scheduler;
mod task;
mod verification;
pub(crate) use agents::FlowdexResumeAgentHandler;
pub(crate) use agents::FlowdexSendMessageHandler;
pub(crate) use agents::FlowdexSpawnAgentHandler;
pub(crate) use agents::FlowdexWaitAgentHandler;
pub(crate) use scheduler::{
    FlowdexQueueTaskHandler, FlowdexSealPhaseHandler, FlowdexStartRunHandler,
    FlowdexWaitRunHandler, QueueFlowdexTaskHandler, SealFlowdexPhaseHandler,
};
pub(crate) use task::FlowdexCreateTaskHandler;
pub(crate) use task::FlowdexTaskIntegrateHandler;
pub(crate) use task::FlowdexTaskRunAgentHandler;
pub(crate) use task::FlowdexTaskVerifyHandler;
pub(crate) use verification::FlowdexVerifyHandler;

const TOOL_NAME: &str = "start_flowdex_workflow";
const WAIT_TOOL_NAME: &str = "wait_flowdex_workflow";

#[derive(Debug, Deserialize)]
struct StartArgs {
    path: String,
    #[serde(default)]
    input: Option<Value>,
}

pub(crate) struct StartFlowdexWorkflowHandler {
    nested_tool_specs: Vec<ToolSpec>,
}

pub(crate) struct WaitFlowdexWorkflowHandler;

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

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
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
        let cell_id = codex_code_mode::CellId::new(args.run_id.clone());
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
        if matches!(pending_activity, Some(InputQueueActivity::Mailbox))
            && session.input_queue.has_trigger_turn_mailbox_items().await
        {
            return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                serde_json::json!({"runId": args.run_id, "status": "message"}).to_string(),
                Some(true),
            )));
        }

        let mut cell_wait = Box::pin(session.services.code_mode_service.wait_until_yield(cell_id));
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
                            .record_ended(response);
                        session
                            .services
                            .code_mode_service
                            .finish_cell_dispatch(runtime_cell_id);
                    }
                    let (result, terminal) = flowdex_result(args.run_id.clone(), wait_response.into());
                    if terminal {
                        session.services.elicitations.wait_until_clear().await;
                    }
                    return Ok(boxed_tool_output(FunctionToolOutput::from_text(result.to_string(), Some(true))));
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    run_id: String,
}

impl StartFlowdexWorkflowHandler {
    pub(crate) fn new(nested_tool_specs: Vec<ToolSpec>) -> Self {
        Self { nested_tool_specs }
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

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl StartFlowdexWorkflowHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
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
        let cwd = turn
            .environments
            .single_local_environment_cwd()
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "start_flowdex_workflow requires one local environment".to_string(),
                )
            })?;
        let loaded = WorkflowLoader::new(cwd)
            .load(Path::new(&args.path), args.input.as_ref())
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let exec = ExecContext { session, turn };
        let (response, cell_id, _started_at) = execute_source(
            &exec,
            call_id,
            loaded.into_source(),
            &self.nested_tool_specs,
            None,
            None,
        )
        .await
        .map_err(FunctionCallError::RespondToModel)?;
        let (result, _) = flowdex_result(cell_id.to_string(), response);
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
