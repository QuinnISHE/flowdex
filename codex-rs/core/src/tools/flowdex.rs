use crate::function_tool::FunctionCallError;
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
mod verification;
pub(crate) use agents::FlowdexSendMessageHandler;
pub(crate) use agents::FlowdexSpawnAgentHandler;
pub(crate) use agents::FlowdexWaitAgentHandler;
pub(crate) use verification::FlowdexVerifyHandler;

const TOOL_NAME: &str = "start_flowdex_workflow";
const PROGRESS_TOOL_NAME: &str = "flowdex_progress";

#[derive(Debug, Deserialize)]
struct StartArgs {
    path: String,
    #[serde(default)]
    input: Option<Value>,
}

pub(crate) struct StartFlowdexWorkflowHandler {
    nested_tool_specs: Vec<ToolSpec>,
}

pub(crate) struct FlowdexProgressHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressArgs {
    summary: String,
}

impl ToolExecutor<ToolInvocation> for FlowdexProgressHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(PROGRESS_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: PROGRESS_TOOL_NAME.to_string(),
            description: "Publish a transient Flowdex progress summary.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([("summary".to_string(), JsonSchema::string(None))]),
                Some(vec!["summary".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_progress(invocation).await.map(boxed_tool_output) })
    }
}

impl CoreToolRuntime for FlowdexProgressHandler {}

async fn handle_progress(
    invocation: ToolInvocation,
) -> Result<FunctionToolOutput, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        ..
    } = invocation;
    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex progress expects JSON arguments".to_string(),
        ));
    };
    let args: ProgressArgs = parse_arguments(&arguments)?;
    let summary = args.summary.trim();
    if summary.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "summary must be non-empty".to_string(),
        ));
    }
    let summary = codex_utils_output_truncation::truncate_text(
        summary,
        codex_utils_output_truncation::TruncationPolicy::Tokens(4096),
    );
    let item = codex_protocol::items::TurnItem::Reasoning(codex_protocol::items::ReasoningItem {
        id: codex_protocol::ResponseItemId::new("rs").to_string(),
        summary_text: vec![summary],
        raw_content: Vec::new(),
    });
    session.emit_flowdex_progress(turn.as_ref(), item).await;
    Ok(FunctionToolOutput::from_text(String::new(), Some(true)))
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
        let (status, content_items, error_text) = match response {
            codex_code_mode::RuntimeResponse::Yielded { content_items, .. } => {
                ("yielded", content_items, None)
            }
            codex_code_mode::RuntimeResponse::Terminated { content_items, .. } => {
                ("terminated", content_items, None)
            }
            codex_code_mode::RuntimeResponse::Result {
                content_items,
                error_text,
                ..
            } => ("completed", content_items, error_text),
        };
        let content_items =
            truncate_code_mode_result(into_function_call_output_content_items(content_items), None);
        let output = function_call_output_content_items_to_text(&content_items).unwrap_or_default();
        let error = error_text.map(|error| {
            let items = truncate_code_mode_result(
                vec![
                    codex_protocol::models::FunctionCallOutputContentItem::InputText {
                        text: error,
                    },
                ],
                None,
            );
            function_call_output_content_items_to_text(&items).unwrap_or_default()
        });
        let mut result = serde_json::json!({
            "runId": cell_id.to_string(),
            "status": if error.is_some() { "failed" } else { status },
            "output": output,
        });
        let success = error.is_none();
        if let Some(error) = error {
            result["error"] = Value::String(error);
        }
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            result.to_string(),
            Some(success),
        )))
    }
}

impl CoreToolRuntime for StartFlowdexWorkflowHandler {}
