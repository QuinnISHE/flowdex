use crate::function_tool::FunctionCallError;
use crate::skills::maybe_emit_implicit_skill_invocation;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::ShellCommandHandler;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::resolve_workdir_base_path;
use crate::tools::handlers::shell::RunExecLikeArgs;
use crate::tools::handlers::shell::RunExecLikeResult;
use crate::tools::handlers::shell::run_exec_like_result;
use crate::tools::handlers::shell::run_shell_command_post_hooks;
use crate::tools::handlers::shell::run_shell_command_pre_hooks;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ShellCommandToolCallParams;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ShellCommandBackendConfig;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const TOOL_NAME: &str = "flowdex_verify";

pub(crate) struct FlowdexVerifyHandler {
    backend: ShellCommandBackendConfig,
}

impl FlowdexVerifyHandler {
    pub(crate) fn new(backend: ShellCommandBackendConfig) -> Self {
        Self { backend }
    }

    pub(crate) async fn handle_for_workdir(
        &self,
        mut invocation: ToolInvocation,
        workdir: &std::path::Path,
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
        self.handle_call(invocation).await
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

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl CoreToolRuntime for FlowdexVerifyHandler {
    fn waits_for_runtime_cancellation(&self) -> bool {
        true
    }
}

impl FlowdexVerifyHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
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

        let Some(turn_environment) = step_context.environments.primary().cloned() else {
            return Err(FunctionCallError::RespondToModel(
                "verification is unavailable in this session".to_string(),
            ));
        };
        let environment_cwd = turn_environment.cwd().to_abs_path().map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "verification cwd `{}` is not native to the Codex host: {err}",
                turn_environment.cwd()
            ))
        })?;
        let cwd = resolve_workdir_base_path(&arguments, &environment_cwd)?;
        let shell_handler = ShellCommandHandler::from(self.backend);
        let shell_runtime_backend = shell_handler.shell_runtime_backend();
        let shell_type = Some(
            turn_environment
                .shell
                .as_ref()
                .map_or_else(|| session.user_shell().shell_type, |shell| shell.shell_type),
        );

        let mut results = Vec::with_capacity(args.commands.len());
        let mut feedback_messages = Vec::new();
        for (index, original_command) in args.commands.iter().enumerate() {
            if cancellation_token.is_cancelled() {
                return Err(FunctionCallError::RespondToModel(
                    "verification cancelled".to_string(),
                ));
            }
            let command_call_id = format!("{call_id}:verify:{index}");
            let command = run_shell_command_pre_hooks(
                &session,
                &turn,
                command_call_id.clone(),
                original_command.clone(),
            )
            .await?;
            maybe_emit_implicit_skill_invocation(session.as_ref(), turn.as_ref(), &command, &cwd)
                .await;
            let params = ShellCommandToolCallParams {
                command: command.clone(),
                workdir: args.workdir.clone(),
                login: None,
                timeout_ms: args.timeout_ms,
                sandbox_permissions: None,
                prefix_rule: None,
                additional_permissions: None,
                justification: None,
            };
            let exec_params = ShellCommandHandler::to_exec_params(
                &params,
                session.as_ref(),
                turn.as_ref(),
                &turn_environment,
                cwd.clone(),
                turn.config.permissions.allow_login_shell,
            )?;
            let result = run_exec_like_result(RunExecLikeArgs {
                tool_name: ToolName::plain(TOOL_NAME),
                exec_params,
                cancellation_token: cancellation_token.clone(),
                hook_command: command.clone(),
                shell_type,
                additional_permissions: None,
                prefix_rule: None,
                session: session.clone(),
                turn: turn.clone(),
                turn_environment: turn_environment.clone(),
                tracker: tracker.clone(),
                call_id: command_call_id.clone(),
                shell_runtime_backend,
            })
            .await?;
            let RunExecLikeResult::Command {
                output: Some(output),
                ..
            } = result
            else {
                return Err(FunctionCallError::RespondToModel(
                    "verification command execution failed".to_string(),
                ));
            };
            let failed = output.exit_code != 0 || output.timed_out;
            let mut entry = serde_json::json!({
                "command": original_command,
                "exitCode": output.exit_code,
                "durationMs": output.duration.as_millis() as u64,
            });
            if output.timed_out {
                entry["timedOut"] = Value::Bool(true);
            }
            if failed && !output.aggregated_output.text.is_empty() {
                entry["output"] = Value::String(output.aggregated_output.text.clone());
            }
            results.push(entry);
            if failed {
                return Ok(boxed_tool_output(VerificationOutput {
                    value: serde_json::json!({"passed": false, "commands": results}),
                    model_output: None,
                }));
            }
            if let Some(feedback) =
                run_shell_command_post_hooks(&session, &turn, command_call_id, command, &output)
                    .await?
            {
                feedback_messages.push(feedback);
            }
        }
        Ok(boxed_tool_output(VerificationOutput {
            value: serde_json::json!({"passed": true, "commands": results}),
            model_output: (!feedback_messages.is_empty()).then(|| feedback_messages.join("\n")),
        }))
    }
}

struct VerificationOutput {
    value: Value,
    model_output: Option<String>,
}

impl ToolOutput for VerificationOutput {
    fn log_preview(&self) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_tools::ShellCommandBackendConfig;

    #[test]
    fn verifier_waits_for_runtime_cancellation() {
        assert!(
            FlowdexVerifyHandler::new(ShellCommandBackendConfig::Classic)
                .waits_for_runtime_cancellation()
        );
    }
}
