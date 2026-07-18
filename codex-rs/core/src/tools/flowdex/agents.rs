use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::agent::control::SpawnAgentCompletionDelivery;
use crate::agent::control::SpawnAgentOptions;
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

pub(crate) struct FlowdexSpawnAgentHandler;
pub(crate) struct FlowdexSendMessageHandler;
pub(crate) struct FlowdexWaitAgentHandler;

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

fn status_value(id: ThreadId, status: AgentStatus) -> Value {
    match status {
        ProtocolAgentStatus::Completed(message) => {
            serde_json::json!({"agentId": id.to_string(), "status": "completed", "message": message.map(|m| truncate_message(&m))})
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
