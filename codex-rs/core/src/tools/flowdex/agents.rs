use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::agent::control::SpawnAgentCompletionDelivery;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::agent::status::is_final;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::apply_requested_spawn_agent_model_overrides;
use crate::tools::handlers::multi_agents_common::apply_spawn_agent_role;
use crate::tools::handlers::multi_agents_common::build_agent_resume_config;
use crate::tools::handlers::multi_agents_common::build_agent_spawn_config;
use crate::tools::handlers::multi_agents_common::collab_agent_error;
use crate::tools::handlers::multi_agents_common::collab_spawn_error;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AgentStatus as ProtocolAgentStatus;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::user_input::UserInput;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const SPAWN_NAME: &str = "flowdex_spawn_agent";
const SEND_NAME: &str = "flowdex_send_message";
const WAIT_NAME: &str = "flowdex_wait_agent";
const RESUME_NAME: &str = "flowdex_resume_agent";
const HANDOFF_PROMPT: &str = "Produce only a concise structured handoff with completed work, current state, relevant files and decisions, remaining work, and verification. Do not modify files or continue implementation.";

pub(crate) struct FlowdexSpawnAgentHandler;
pub(crate) struct FlowdexSendMessageHandler;
pub(crate) struct FlowdexWaitAgentHandler;
pub(crate) struct FlowdexResumeAgentHandler;

impl ToolExecutor<ToolInvocation> for FlowdexSpawnAgentHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SPAWN_NAME)
    }
    fn spec(&self) -> ToolSpec {
        spawn_spec()
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_spawn(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for FlowdexSpawnAgentHandler {}

impl ToolExecutor<ToolInvocation> for FlowdexSendMessageHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SEND_NAME)
    }
    fn spec(&self) -> ToolSpec {
        send_spec()
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_send(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for FlowdexSendMessageHandler {}

impl ToolExecutor<ToolInvocation> for FlowdexWaitAgentHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(WAIT_NAME)
    }
    fn spec(&self) -> ToolSpec {
        wait_spec()
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_wait(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for FlowdexWaitAgentHandler {}

impl ToolExecutor<ToolInvocation> for FlowdexResumeAgentHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(RESUME_NAME)
    }
    fn spec(&self) -> ToolSpec {
        resume_spec()
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_resume(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for FlowdexResumeAgentHandler {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    name: String,
    instructions: String,
    profile: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

async fn handle_spawn(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        ..
    } = invocation;
    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex spawn expects JSON arguments".into(),
        ));
    };
    let args: SpawnArgs = parse_arguments(&arguments)?;
    let name = args.name.trim();
    let instructions = args.instructions.trim();
    if name.is_empty() || instructions.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "name and instructions must be non-empty".into(),
        ));
    }
    let profile = args
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if profile.is_none() && args.model.is_none() && args.reasoning_effort.is_none() {
        return Err(FunctionCallError::RespondToModel(
            "spawnAgent requires profile, model, or reasoningEffort".into(),
        ));
    }

    let child_depth = next_thread_spawn_depth(&turn.session_source);
    if exceeds_thread_spawn_depth_limit(child_depth, turn.config.agent_max_depth) {
        return Err(FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".to_string(),
        ));
    }
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
    apply_requested_spawn_agent_model_overrides(
        &session,
        turn.as_ref(),
        &mut config,
        args.model.as_deref(),
        args.reasoning_effort,
    )
    .await?;
    apply_spawn_agent_role(&session, &mut config, profile).await?;
    let spawn_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        profile,
        Some(name.to_string()),
    )?;
    let child_path = spawn_source.get_agent_path().ok_or_else(|| {
        FunctionCallError::RespondToModel("spawned agent is missing an agent path".into())
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = InterAgentCommunication::new_encrypted(
        author,
        child_path,
        Vec::new(),
        instructions.to_string(),
        true,
    );
    let context = AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id);
    let child = session
        .services
        .agent_control
        .spawn_agent_with_communication(
            config,
            communication,
            context,
            Some(spawn_source),
            SpawnAgentOptions {
                parent_thread_id: Some(session.thread_id),
                environments: Some(turn.environments.to_selections()),
                completion_delivery: SpawnAgentCompletionDelivery::StatusOnly,
                ..Default::default()
            },
        )
        .await
        .map_err(collab_spawn_error)?;
    Ok(JsonOutput::new(Value::String(child.thread_id.to_string())))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    agent_id: String,
    message: String,
    delivery: Option<String>,
}

async fn handle_send(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        ..
    } = invocation;
    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex sendMessage expects JSON arguments".into(),
        ));
    };
    let args: SendArgs = parse_arguments(&arguments)?;
    if args.message.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "message must be non-empty".into(),
        ));
    }
    let trigger_turn = match args.delivery.as_deref().unwrap_or("queue") {
        "queue" => false,
        "turn" => true,
        other => {
            return Err(FunctionCallError::RespondToModel(format!(
                "delivery must be `queue` or `turn`, got `{other}`"
            )));
        }
    };
    let target = resolve_agent_target(&session, &turn, &args.agent_id).await?;
    let metadata = session
        .services
        .agent_control
        .ensure_agent_known(target)
        .map_err(|err| collab_agent_error(target, err))?;
    let receiver = metadata.agent_path.ok_or_else(|| {
        FunctionCallError::RespondToModel("target agent is missing an agent_path".into())
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = InterAgentCommunication::new_encrypted(
        author,
        receiver,
        Vec::new(),
        args.message,
        trigger_turn,
    );
    let context = AgentCommunicationContext::new(
        if trigger_turn {
            AgentCommunicationKind::Followup
        } else {
            AgentCommunicationKind::Message
        },
        session.thread_id,
    );
    let submission_id = session
        .services
        .agent_control
        .send_inter_agent_communication(target, communication, context)
        .await
        .map_err(|err| collab_agent_error(target, err))?;
    Ok(JsonOutput::new(
        serde_json::json!({"submissionId": submission_id}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    agent_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeArgs {
    agent_id: String,
    instructions: String,
    #[serde(default)]
    options: Option<ResumeOptions>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeOptions {
    #[serde(default)]
    context_mode: Option<String>,
}

async fn handle_resume(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        ..
    } = invocation;
    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex resumeAgent expects JSON arguments".into(),
        ));
    };
    let args: ResumeArgs = parse_arguments(&arguments)?;
    let instructions = args.instructions.trim();
    if instructions.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "instructions must be non-empty".into(),
        ));
    }
    let mode = args
        .options
        .and_then(|options| options.context_mode)
        .unwrap_or_else(|| "keep".to_string());
    if !matches!(mode.as_str(), "keep" | "compact" | "handoff") {
        return Err(FunctionCallError::RespondToModel(format!(
            "unknown context mode `{mode}`"
        )));
    }
    let id = ThreadId::from_string(&args.agent_id)
        .map_err(|_| FunctionCallError::RespondToModel("agentId must be a thread id".into()))?;
    let status_rx = match session.services.agent_control.subscribe_status(id).await {
        Ok(rx) => rx,
        Err(codex_protocol::error::CodexErr::ThreadNotFound(_)) => {
            return Ok(JsonOutput::new(
                serde_json::json!({"agentId": id.to_string(), "status": "notFound"}),
            ));
        }
        Err(err) => return Err(collab_agent_error(id, err)),
    };
    if !is_final(&status_rx.borrow()) {
        return Err(FunctionCallError::RespondToModel(
            "agent must have a completed prior turn".into(),
        ));
    }

    if mode == "compact" {
        session
            .services
            .agent_control
            .submit_compaction(id)
            .await
            .map_err(|err| collab_agent_error(id, err))?;
        let compact_status = session
            .services
            .agent_control
            .wait_for_submitted_operation(id, status_rx)
            .await;
        if !matches!(compact_status, AgentStatus::Completed(_)) {
            return Ok(JsonOutput::new(status_value(id, compact_status)));
        }
        let status_rx = session
            .services
            .agent_control
            .subscribe_status(id)
            .await
            .map_err(|err| collab_agent_error(id, err))?;
        let status = submit_trigger_turn(&session, &turn, id, instructions, status_rx).await?;
        return Ok(JsonOutput::new(status_value(id, status)));
    }

    if mode == "handoff" {
        let handoff_status =
            submit_trigger_turn(&session, &turn, id, HANDOFF_PROMPT, status_rx).await?;
        let handoff_text = match handoff_status {
            AgentStatus::Completed(Some(text)) if !text.trim().is_empty() => text,
            status => return Ok(JsonOutput::new(status_value(id, status))),
        };
        let snapshot = session
            .services
            .agent_control
            .get_agent_config_snapshot(id)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel("agent configuration unavailable".into())
            })?;
        let mut config = build_agent_resume_config(turn.as_ref())?;
        config.model = Some(snapshot.model.clone());
        config.model_provider_id = snapshot.model_provider_id.clone();
        config.service_tier = snapshot.service_tier.clone();
        config.personality = snapshot.personality;
        config.cwd = snapshot.cwd().clone();
        config
            .permissions
            .set_workspace_roots(snapshot.workspace_roots.clone());
        config
            .permissions
            .set_permission_profile(snapshot.permission_profile.clone())
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let parent_id = snapshot.parent_thread_id.unwrap_or(session.thread_id);
        let depth = match &snapshot.session_source {
            codex_protocol::protocol::SessionSource::SubAgent(
                codex_protocol::protocol::SubAgentSource::ThreadSpawn { depth, .. },
            ) => *depth,
            _ => next_thread_spawn_depth(&turn.session_source),
        };
        let replacement_name = format!("handoff_{}", ThreadId::new().to_string().replace('-', ""));
        let source = thread_spawn_source(
            parent_id,
            &snapshot.session_source,
            depth,
            snapshot.session_source.get_agent_role().as_deref(),
            Some(replacement_name),
        )?;
        let prompt = format!(
            "Handoff:\n{}\n\nInstructions:\n{}",
            truncate_message(&handoff_text),
            instructions
        );
        let child = session
            .services
            .agent_control
            .spawn_agent_with_metadata(
                config,
                vec![UserInput::Text {
                    text: prompt,
                    text_elements: Vec::new(),
                }],
                Some(source),
                SpawnAgentOptions {
                    parent_thread_id: Some(parent_id),
                    environments: Some(snapshot.environment_selections().to_vec()),
                    completion_delivery: SpawnAgentCompletionDelivery::StatusOnly,
                    ..Default::default()
                },
            )
            .await
            .map_err(collab_spawn_error)?;
        let replacement_id = child.thread_id;
        let replacement_status = wait_for_terminal(&session, replacement_id).await;
        return Ok(JsonOutput::new(status_value(
            replacement_id,
            replacement_status,
        )));
    }

    let status_rx = session
        .services
        .agent_control
        .subscribe_status(id)
        .await
        .map_err(|err| collab_agent_error(id, err))?;
    let status = submit_trigger_turn(&session, &turn, id, instructions, status_rx).await?;
    Ok(JsonOutput::new(status_value(id, status)))
}

async fn submit_trigger_turn(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    id: ThreadId,
    message: &str,
    status_rx: tokio::sync::watch::Receiver<AgentStatus>,
) -> Result<AgentStatus, FunctionCallError> {
    let metadata = session
        .services
        .agent_control
        .ensure_agent_known(id)
        .map_err(|err| collab_agent_error(id, err))?;
    let receiver = metadata.agent_path.ok_or_else(|| {
        FunctionCallError::RespondToModel("target agent is missing an agent_path".into())
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = InterAgentCommunication::new_encrypted(
        author,
        receiver,
        Vec::new(),
        message.to_string(),
        true,
    );
    let context =
        AgentCommunicationContext::new(AgentCommunicationKind::Followup, session.thread_id);
    session
        .services
        .agent_control
        .send_inter_agent_communication(id, communication, context)
        .await
        .map_err(|err| collab_agent_error(id, err))?;
    Ok(session
        .services
        .agent_control
        .wait_for_submitted_operation(id, status_rx)
        .await)
}

async fn wait_for_terminal(
    session: &std::sync::Arc<crate::session::session::Session>,
    id: ThreadId,
) -> AgentStatus {
    let Ok(mut rx) = session.services.agent_control.subscribe_status(id).await else {
        return AgentStatus::NotFound;
    };
    let mut status = rx.borrow().clone();
    while !is_final(&status) {
        if rx.changed().await.is_err() {
            return session.services.agent_control.get_status(id).await;
        }
        status = rx.borrow().clone();
    }
    status
}

async fn handle_wait(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let ToolInvocation {
        session, payload, ..
    } = invocation;
    let ToolPayload::Function { arguments } = payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex waitAgent expects JSON arguments".into(),
        ));
    };
    let args: WaitArgs = parse_arguments(&arguments)?;
    let id = ThreadId::from_string(&args.agent_id)
        .map_err(|_| FunctionCallError::RespondToModel("agentId must be a thread id".into()))?;
    let mut rx = match session.services.agent_control.subscribe_status(id).await {
        Ok(rx) => rx,
        Err(codex_protocol::error::CodexErr::ThreadNotFound(_)) => {
            return Ok(JsonOutput::new(
                serde_json::json!({"agentId": id.to_string(), "status": "notFound"}),
            ));
        }
        Err(err) => return Err(collab_agent_error(id, err)),
    };
    let mut status = rx.borrow().clone();
    while !is_final(&status) {
        if rx.changed().await.is_err() {
            status = session.services.agent_control.get_status(id).await;
            break;
        }
        status = rx.borrow().clone();
    }
    Ok(JsonOutput::new(status_value(id, status)))
}

pub(crate) fn status_value(id: ThreadId, status: AgentStatus) -> Value {
    match status {
        ProtocolAgentStatus::Completed(Some(message)) => serde_json::json!({
            "agentId": id.to_string(),
            "status": "completed",
            "message": truncate_message(&message),
        }),
        ProtocolAgentStatus::Completed(None) => {
            serde_json::json!({"agentId": id.to_string(), "status": "completed"})
        }
        ProtocolAgentStatus::Errored(error) => {
            serde_json::json!({"agentId": id.to_string(), "status": "errored", "message": truncate_message(&error)})
        }
        ProtocolAgentStatus::Shutdown => {
            serde_json::json!({"agentId": id.to_string(), "status": "shutdown"})
        }
        ProtocolAgentStatus::NotFound => {
            serde_json::json!({"agentId": id.to_string(), "status": "notFound"})
        }
        _ => {
            serde_json::json!({"agentId": id.to_string(), "status": "errored", "message": "agent status ended unexpectedly"})
        }
    }
}

fn truncate_message(message: &str) -> String {
    codex_utils_output_truncation::truncate_text(
        message,
        codex_utils_output_truncation::TruncationPolicy::Tokens(4096),
    )
}

struct JsonOutput {
    value: Value,
}
impl JsonOutput {
    fn new(value: Value) -> Self {
        Self { value }
    }
}
impl ToolOutput for JsonOutput {
    fn log_preview(&self) -> String {
        self.value.to_string()
    }
    fn success_for_logging(&self) -> bool {
        true
    }
    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(self.value.to_string()),
                success: Some(true),
            },
        }
    }
    fn code_mode_result(&self, _payload: &ToolPayload) -> Value {
        self.value.clone()
    }
}

fn required(properties: BTreeMap<String, JsonSchema>, fields: Vec<String>) -> JsonSchema {
    JsonSchema::object(properties, Some(fields), Some(false.into()))
}
fn spawn_spec() -> ToolSpec {
    let properties = BTreeMap::from([
        ("name".into(), JsonSchema::string(None)),
        ("instructions".into(), JsonSchema::string(None)),
        ("profile".into(), JsonSchema::string(None)),
        ("model".into(), JsonSchema::string(None)),
        ("reasoning_effort".into(), JsonSchema::string(None)),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: SPAWN_NAME.into(),
        description: "Spawn a Flowdex sub-agent.".into(),
        strict: false,
        defer_loading: None,
        parameters: required(properties, vec!["name".into(), "instructions".into()]),
        output_schema: Some(serde_json::json!({"type":"string"})),
    })
}
fn send_spec() -> ToolSpec {
    let properties = BTreeMap::from([
        ("agent_id".into(), JsonSchema::string(None)),
        ("message".into(), JsonSchema::string(None)),
        ("delivery".into(), JsonSchema::string(None)),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: SEND_NAME.into(),
        description: "Send a message to a Flowdex sub-agent.".into(),
        strict: false,
        defer_loading: None,
        parameters: required(properties, vec!["agent_id".into(), "message".into()]),
        output_schema: Some(serde_json::json!({"type":"object"})),
    })
}
fn wait_spec() -> ToolSpec {
    let properties = BTreeMap::from([("agent_id".into(), JsonSchema::string(None))]);
    ToolSpec::Function(ResponsesApiTool {
        name: WAIT_NAME.into(),
        description: "Wait for a Flowdex sub-agent to finish.".into(),
        strict: false,
        defer_loading: None,
        parameters: required(properties, vec!["agent_id".into()]),
        output_schema: Some(serde_json::json!({"type":"object"})),
    })
}

fn resume_spec() -> ToolSpec {
    let options = JsonSchema::object(
        BTreeMap::from([("context_mode".into(), JsonSchema::string(None))]),
        Some(vec![]),
        Some(false.into()),
    );
    let properties = BTreeMap::from([
        ("agent_id".into(), JsonSchema::string(None)),
        ("instructions".into(), JsonSchema::string(None)),
        ("options".into(), options),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: RESUME_NAME.into(),
        description: "Resume a Flowdex sub-agent with an explicit context mode.".into(),
        strict: false,
        defer_loading: None,
        parameters: required(properties, vec!["agent_id".into(), "instructions".into()]),
        output_schema: Some(serde_json::json!({"type":"object"})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_without_output_omits_message() {
        let result = status_value(ThreadId::new(), ProtocolAgentStatus::Completed(None));

        assert_eq!(result["status"], "completed");
        assert!(result.get("message").is_none());
    }
}
