use super::rules::run_rules;
use crate::environment_selection::TurnEnvironmentState;
use crate::function_tool::FunctionCallError;
use crate::session::step_context::StepContext;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::ExecCommandHandler;
use crate::tools::handlers::ExecCommandHandlerOptions;
use crate::tools::handlers::parse_arguments;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::PostToolUsePayload;
use crate::tools::registry::PreToolUsePayload;
use crate::tools::registry::ToolExecutor;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

const TOOL_NAME: &str = "flowdex_verify";
const EXEC_TOOL_NAME: &str = "flowdex_exec_command";

pub(crate) struct FlowdexVerifyHandler;

pub(crate) struct FlowdexExecCommandHandler {
    inner: ExecCommandHandler,
}

impl FlowdexExecCommandHandler {
    pub(crate) fn new(options: ExecCommandHandlerOptions) -> Self {
        Self {
            inner: ExecCommandHandler::one_shot(options),
        }
    }
}

impl ToolExecutor<ToolInvocation> for FlowdexExecCommandHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(EXEC_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let ToolSpec::Function(mut spec) = self.inner.spec() else {
            unreachable!("exec_command uses a function spec")
        };
        spec.name = EXEC_TOOL_NAME.to_string();
        ToolSpec::Function(spec)
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.inner.supports_parallel_tool_calls()
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        self.inner.handle(invocation)
    }
}

impl CoreToolRuntime for FlowdexExecCommandHandler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        self.inner.matches_kind(payload)
    }

    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        self.inner.pre_tool_use_payload(invocation)
    }

    fn with_updated_hook_input(
        &self,
        invocation: ToolInvocation,
        updated_input: Value,
    ) -> Result<ToolInvocation, FunctionCallError> {
        self.inner
            .with_updated_hook_input(invocation, updated_input)
    }

    fn post_tool_use_payload(
        &self,
        invocation: &ToolInvocation,
        result: &dyn ToolOutput,
    ) -> Option<PostToolUsePayload> {
        self.inner.post_tool_use_payload(invocation, result)
    }
}

impl FlowdexVerifyHandler {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) async fn handle_for_workdir(
        &self,
        mut invocation: ToolInvocation,
        workdir: &std::path::Path,
        runtime_cwd: Option<&AbsolutePathBuf>,
        trusted_repository_root: Option<&std::path::Path>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let ToolPayload::Function { arguments } = &invocation.payload else {
            return Err(FunctionCallError::RespondToModel(
                "flowdex verify expects JSON arguments".to_string(),
            ));
        };
        let mut value: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        value["workdir"] = serde_json::Value::String(workdir.to_string_lossy().into_owned());
        invocation.payload = ToolPayload::Function {
            arguments: value.to_string(),
        };
        self.handle_call(invocation, runtime_cwd, trusted_repository_root)
            .await
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyArgs {
    commands: Vec<String>,
    workdir: Option<String>,
    timeout_ms: Option<u64>,
}

impl ToolExecutor<ToolInvocation> for FlowdexVerifyHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let properties = BTreeMap::from([
            (
                "commands".to_string(),
                JsonSchema::array(JsonSchema::string(None), None),
            ),
            ("workdir".to_string(), JsonSchema::string(None)),
            ("timeout_ms".to_string(), JsonSchema::number(None)),
        ]);
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: "Run ordered verification commands for a Flowdex workflow.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["commands".to_string()]),
                Some(false.into()),
            ),
            output_schema: Some(serde_json::json!({"type": "object"})),
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation, None, None))
    }
}

impl CoreToolRuntime for FlowdexVerifyHandler {}

impl FlowdexVerifyHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
        runtime_cwd: Option<&AbsolutePathBuf>,
        trusted_repository_root: Option<&std::path::Path>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            step_context,
            cancellation_token,
            tracker,
            call_id,
            payload,
            ..
        } = invocation;
        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "flowdex verify expects JSON arguments".to_string(),
            ));
        };
        let args: VerifyArgs = parse_arguments(&arguments)?;
        if args.commands.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "commands must be a non-empty array".to_string(),
            ));
        }
        if args
            .commands
            .iter()
            .any(|command| command.trim().is_empty())
        {
            return Err(FunctionCallError::RespondToModel(
                "commands must contain only non-empty strings".to_string(),
            ));
        }
        let timeout_ms = verification_timeout_ms(
            args.timeout_ms,
            turn.config.flowdex_config.verification_timeout_ms,
        );

        let Some(mut turn_environment) = step_context.environments.primary().cloned() else {
            return Err(FunctionCallError::RespondToModel(
                "verification is unavailable in this session".to_string(),
            ));
        };
        if let Some(runtime_cwd) = runtime_cwd {
            turn_environment.set_runtime_cwd(PathUri::from_abs_path(runtime_cwd));
            let EnvironmentConfigState::Ready(environment_config) =
                &mut turn_environment.selection.config
            else {
                unreachable!("ready turn environments always carry resolved configuration")
            };
            environment_config.permission_profile = turn
                .config
                .permissions
                .permission_profile_state()
                .snapshot();
        }
        let cwd = turn_environment.cwd().to_abs_path().map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "verification cwd `{}` is not native to the Codex host: {err}",
                turn_environment.cwd()
            ))
        })?;
        let mut environments = step_context.environments.clone();
        let Some(primary) = environments
            .environments
            .iter_mut()
            .find(|environment| matches!(environment, TurnEnvironmentState::Ready(_)))
        else {
            return Err(FunctionCallError::RespondToModel(
                "verification is unavailable in this session".to_string(),
            ));
        };
        *primary = TurnEnvironmentState::Ready(turn_environment);
        let verification_step = Arc::new(StepContext {
            turn: Arc::clone(&turn),
            settings: Arc::clone(&step_context.settings),
            token_budget: step_context.token_budget.clone(),
            session_telemetry: step_context.session_telemetry.clone(),
            environments,
            selected_capability_roots: step_context.selected_capability_roots.clone(),
            executor_capability_discovery: step_context.executor_capability_discovery.clone(),
            mcp: Arc::clone(&step_context.mcp),
            tool_router: Arc::clone(&step_context.tool_router),
            loaded_agents_md: step_context.loaded_agents_md.clone(),
        });

        let mut results = Vec::with_capacity(args.commands.len());
        for (index, original_command) in args.commands.iter().enumerate() {
            if cancellation_token.is_cancelled() {
                return Err(FunctionCallError::RespondToModel(
                    "verification cancelled".to_string(),
                ));
            }
            let command_call_id = format!("{call_id}:verify:{index}");
            let payload = ToolPayload::Function {
                arguments: serde_json::json!({
                    "cmd": original_command,
                    "workdir": args.workdir.clone(),
                    "timeout_ms": timeout_ms,
                    "tty": false,
                })
                .to_string(),
            };
            let result = ToolCallRuntime::new(
                Arc::clone(&session),
                Arc::clone(&verification_step),
                Arc::clone(&tracker),
            )
            .handle_tool_call_with_source(
                ToolCall {
                    tool_name: ToolName::plain(EXEC_TOOL_NAME),
                    call_id: command_call_id,
                    payload,
                    encrypted_function_args: None,
                },
                ToolCallSource::Direct,
                cancellation_token.clone(),
            )
            .await?;
            let command_result = result.code_mode_result();
            let exit_code = command_result
                .get("exit_code")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "verification command execution did not return an exit code".to_string(),
                    )
                })?;
            let timed_out = exit_code == 124;
            let failed = exit_code != 0;
            let mut entry = serde_json::json!({
                "command": original_command,
                "exitCode": exit_code,
                "durationMs": command_result
                    .get("wall_time_seconds")
                    .and_then(Value::as_f64)
                    .map(|seconds| (seconds * 1000.0).round() as u64)
                    .unwrap_or_default(),
            });
            if timed_out {
                entry["timedOut"] = Value::Bool(true);
            }
            if failed
                && let Some(output) = command_result.get("output").and_then(Value::as_str)
                && !output.is_empty()
            {
                entry["output"] = Value::String(output.to_string());
            }
            results.push(entry);
            if failed {
                return Ok(boxed_tool_output(VerificationOutput {
                    value: serde_json::json!({"passed": false, "commands": results}),
                    model_output: None,
                }));
            }
        }
        let mut value = serde_json::json!({"passed": true, "commands": results});
        let rule_ids = &turn.config.flowdex_config.ast_grep_always_run;
        if !rule_ids.is_empty() {
            if !turn.config.active_project.is_trusted() {
                return Err(FunctionCallError::RespondToModel(
                    "Flowdex rules require a trusted Git repository".to_string(),
                ));
            }
            let rules = run_rules(
                trusted_repository_root.unwrap_or(turn.config.cwd.as_path()),
                cwd.as_path(),
                rule_ids.clone(),
                &cancellation_token,
            )
            .await?;
            value["rules"] = serde_json::to_value(&rules)
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            if !rules.passed {
                value["passed"] = Value::Bool(false);
            }
        }
        Ok(boxed_tool_output(VerificationOutput {
            value,
            model_output: None,
        }))
    }
}

struct VerificationOutput {
    value: Value,
    model_output: Option<String>,
}

impl ToolOutput for VerificationOutput {
    fn log_output(&self) -> String {
        self.value.to_string()
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                body: codex_protocol::models::FunctionCallOutputBody::Text(
                    self.model_output
                        .clone()
                        .unwrap_or_else(|| self.value.to_string()),
                ),
                success: Some(true),
            },
        }
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> Value {
        self.value.clone()
    }
}

fn verification_timeout_ms(explicit: Option<u64>, configured: u64) -> u64 {
    explicit.unwrap_or(configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_timeout_uses_config_unless_explicitly_overridden() {
        assert_eq!(verification_timeout_ms(None, 300_000), 300_000);
        assert_eq!(verification_timeout_ms(Some(900_000), 300_000), 900_000);
    }
}
