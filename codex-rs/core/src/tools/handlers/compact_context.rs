use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::compact_context_spec::COMPACT_CONTEXT_TOOL_NAME;
use crate::tools::handlers::compact_context_spec::create_compact_context_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

pub(crate) const COMPACT_CONTEXT_MESSAGE: &str = "Context compaction scheduled.";

pub struct CompactContextHandler;

impl ToolExecutor<ToolInvocation> for CompactContextHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(COMPACT_CONTEXT_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_compact_context_tool()
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolPayload::Function { arguments } = invocation.payload else {
                return Err(FunctionCallError::RespondToModel(
                    "compact_context handler received unsupported payload".to_string(),
                ));
            };
            let arguments: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&arguments).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "compact_context requires an empty object: {err}"
                    ))
                })?;
            if !arguments.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "compact_context requires an empty object".to_string(),
                ));
            }

            invocation.session.request_compact().await;

            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                COMPACT_CONTEXT_MESSAGE.to_string(),
                Some(true),
            )))
        })
    }
}

impl CoreToolRuntime for CompactContextHandler {}
