use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

use super::ExecContext;
use super::PUBLIC_TOOL_NAME;
use super::handle_runtime_response;
use super::is_exec_tool_name;
use super::telemetry::CodeModeToolCallGuard;

pub struct CodeModeExecuteHandler {
    spec: ToolSpec,
    nested_tool_specs: Vec<ToolSpec>,
}

impl CodeModeExecuteHandler {
    pub(crate) fn new(spec: ToolSpec, nested_tool_specs: Vec<ToolSpec>) -> Self {
        Self {
            spec,
            nested_tool_specs,
        }
    }

    async fn execute(
        &self,
        session: std::sync::Arc<crate::session::session::Session>,
        turn: std::sync::Arc<crate::session::turn_context::TurnContext>,
        call_id: String,
        code: String,
        telemetry: &mut CodeModeToolCallGuard,
    ) -> Result<FunctionToolOutput, FunctionCallError> {
        let args =
            codex_code_mode::parse_exec_source(&code).map_err(FunctionCallError::RespondToModel)?;
        let exec = ExecContext { session, turn };
        let (response, cell_id, started_at) = execute_source(
            &exec,
            call_id,
            args.code,
            &self.nested_tool_specs,
            args.yield_time_ms,
            args.max_output_tokens,
        )
        .await
        .map_err(FunctionCallError::RespondToModel)?;
        telemetry.cell_id = Some(cell_id.to_string());
        handle_runtime_response(&exec, response, args.max_output_tokens, started_at)
            .await
            .map_err(FunctionCallError::RespondToModel)
    }
}

pub(crate) async fn execute_source(
    exec: &ExecContext,
    call_id: String,
    source: String,
    nested_tool_specs: &[ToolSpec],
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
) -> Result<
    (
        codex_code_mode::RuntimeResponse,
        codex_code_mode::CellId,
        std::time::Instant,
    ),
    String,
> {
    execute_source_with_cell_hook(
        exec,
        call_id,
        source,
        nested_tool_specs,
        yield_time_ms,
        max_output_tokens,
        None,
        None,
    )
    .await
}

pub(crate) async fn execute_source_with_cell_hook(
    exec: &ExecContext,
    call_id: String,
    source: String,
    nested_tool_specs: &[ToolSpec],
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
    dispatch_parent_cell: Option<codex_code_mode::CellId>,
    cell_started: Option<Box<dyn FnOnce(&codex_code_mode::CellId) + Send>>,
) -> Result<
    (
        codex_code_mode::RuntimeResponse,
        codex_code_mode::CellId,
        std::time::Instant,
    ),
    String,
> {
    let enabled_tools = codex_tools::collect_code_mode_tool_definitions(nested_tool_specs);
    let started_at = std::time::Instant::now();
    let started_cell = exec
        .session
        .services
        .code_mode_service
        .execute(codex_code_mode::ExecuteRequest {
            tool_call_id: call_id.clone(),
            enabled_tools,
            source: source.clone(),
            yield_time_ms,
            max_output_tokens,
        })
        .await?;
    let cell_id = started_cell.cell_id.clone();
    exec.session
        .services
        .analytics_events_client
        .track_code_mode_tool_call(codex_analytics::CodeModeToolCallFact::CellStarted {
            thread_id: exec.session.thread_id.to_string(),
            turn_id: exec.turn.sub_id.clone(),
            call_id: call_id.clone(),
            cell_id: cell_id.to_string(),
        });
    if let Some(executed_tool_calls) = exec.session.services.executed_tool_calls.as_ref() {
        executed_tool_calls.register_cell(&cell_id, &call_id);
    }
    if let Some(cell_started) = cell_started {
        cell_started(&cell_id);
    }
    let runtime_cell_id = cell_id.to_string();
    let code_cell_trace = exec
        .session
        .services
        .rollout_thread_trace
        .start_code_cell_trace(
            exec.turn.sub_id.as_str(),
            runtime_cell_id.as_str(),
            call_id.as_str(),
            source.as_str(),
        );
    exec.session
        .services
        .code_mode_service
        .mark_cell_ready_for_dispatch(&cell_id, dispatch_parent_cell.as_ref())?;
    let response = started_cell.initial_response().await?;
    code_cell_trace.record_initial_response(&response);
    if !matches!(response, codex_code_mode::RuntimeResponse::Yielded { .. }) {
        code_cell_trace.record_ended(&response);
        exec.session
            .services
            .code_mode_service
            .finish_cell_dispatch(&cell_id);
        exec.session
            .services
            .analytics_events_client
            .track_code_mode_tool_call(codex_analytics::CodeModeToolCallFact::CellClosed {
                thread_id: exec.session.thread_id.to_string(),
                turn_id: exec.turn.sub_id.clone(),
                cell_id: cell_id.to_string(),
            });
    }
    exec.session.services.elicitations.wait_until_clear().await;
    Ok((response, cell_id, started_at))
}

impl ToolExecutor<ToolInvocation> for CodeModeExecuteHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(PUBLIC_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl CodeModeExecuteHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            tool_name,
            payload,
            ..
        } = invocation;

        let mut telemetry = CodeModeToolCallGuard::new(
            session.services.analytics_events_client.clone(),
            session.thread_id.to_string(),
            turn.sub_id.clone(),
            call_id.clone(),
            PUBLIC_TOOL_NAME,
        );
        let result = match payload {
            ToolPayload::Custom { input } if is_exec_tool_name(&tool_name) => self
                .execute(session, turn, call_id, input, &mut telemetry)
                .await
                .map(boxed_tool_output),
            _ => Err(FunctionCallError::RespondToModel(format!(
                "{PUBLIC_TOOL_NAME} expects raw JavaScript source text"
            ))),
        };
        telemetry.finish(
            result
                .as_ref()
                .is_ok_and(|output| output.success_for_logging()),
        );
        result
    }
}

impl CoreToolRuntime for CodeModeExecuteHandler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Custom { .. })
    }
}
