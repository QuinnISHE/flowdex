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
use crate::session::SessionSettingsUpdate;
use crate::session::step_settings::StepSettingsUpdate;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::apply_explicit_spawn_agent_model_overrides;
use crate::tools::handlers::multi_agents_common::apply_spawn_agent_role;
use crate::tools::handlers::multi_agents_common::apply_spawn_agent_runtime_overrides;
use crate::tools::handlers::multi_agents_common::build_agent_spawn_config;
use crate::tools::handlers::multi_agents_common::collab_agent_error;
use crate::tools::handlers::multi_agents_common::collab_spawn_error;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::items::CollabAgentTool;
use codex_protocol::items::CollabAgentToolCallItem;
use codex_protocol::items::CollabAgentToolCallStatus;
use codex_protocol::items::TurnItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AgentStatus as ProtocolAgentStatus;
use codex_protocol::protocol::CollabAgentRef;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::user_input::UserInput;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::config::{Config, ConfigOverrides, deserialize_config_toml_with_base};
use codex_config::{ConfigLayerEntry, ConfigLayerSource, ConfigLayerStack};
use codex_exec_server::LOCAL_FS;
use toml::Value as TomlValue;
use uuid::Uuid;

const SPAWN_NAME: &str = "flowdex_spawn_agent";
const SEND_NAME: &str = "flowdex_send_message";
const WAIT_NAME: &str = "flowdex_wait_agent";
const RESUME_NAME: &str = "flowdex_resume_agent";
const HANDOFF_PROMPT: &str = "Produce only a concise structured handoff with completed work, current state, relevant files and decisions, remaining work, and verification. Do not modify files or continue implementation.";

pub(super) struct FlowdexSpawnUiEvent {
    id: String,
    prompt: String,
}

pub(super) async fn begin_spawn_ui_event(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    prompt: String,
) -> FlowdexSpawnUiEvent {
    let event = FlowdexSpawnUiEvent {
        id: Uuid::new_v4().to_string(),
        prompt,
    };
    session
        .emit_flowdex_item_started(
            turn,
            TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                id: event.id.clone(),
                tool: CollabAgentTool::SpawnAgent,
                status: CollabAgentToolCallStatus::InProgress,
                sender_thread_id: session.thread_id,
                receiver_thread_ids: Vec::new(),
                receiver_agents: Vec::new(),
                prompt: Some(event.prompt.clone()),
                model: None,
                reasoning_effort: None,
                agents_states: Default::default(),
            }),
        )
        .await;
    event
}

pub(super) async fn finish_spawn_ui_event(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    event: FlowdexSpawnUiEvent,
    agent_id: Option<ThreadId>,
    nickname: Option<String>,
    role: Option<String>,
    agent_status: AgentStatus,
) {
    let receiver_thread_ids = agent_id.into_iter().collect();
    let receiver_agents = agent_id
        .map(|thread_id| CollabAgentRef {
            thread_id,
            agent_nickname: nickname,
            agent_role: role,
        })
        .into_iter()
        .collect();
    let agents_states = agent_id
        .map(|thread_id| [(thread_id, agent_status.clone())].into_iter().collect())
        .unwrap_or_default();
    session
        .emit_flowdex_item_completed(
            turn,
            TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                id: event.id,
                tool: CollabAgentTool::SpawnAgent,
                status: if agent_id.is_some() {
                    CollabAgentToolCallStatus::Completed
                } else {
                    CollabAgentToolCallStatus::Failed
                },
                sender_thread_id: session.thread_id,
                receiver_thread_ids,
                receiver_agents,
                prompt: Some(event.prompt),
                model: None,
                reasoning_effort: None,
                agents_states,
            }),
        )
        .await;
}

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
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
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
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
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
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
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
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
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
    tool_profile: Option<String>,
}

pub(crate) async fn apply_flowdex_tool_profile(
    _session: &crate::session::session::Session,
    config: &mut Config,
    profile_name: Option<&str>,
) -> Result<(), FunctionCallError> {
    let profile = profile_name
        .map(|profile_name| {
            config
                .flowdex_config
                .tool_profiles
                .get(profile_name)
                .cloned()
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(format!(
                        "unknown Flowdex tool profile `{profile_name}`"
                    ))
                })
        })
        .transpose()?;
    let mut excluded_tools = config.flowdex_config.subagent_excluded_tools.clone();
    let mut excluded_skills = config.flowdex_config.subagent_excluded_skills.clone();
    if let Some(profile) = profile.as_ref() {
        excluded_tools.extend(profile.excluded_tools.iter().cloned());
        excluded_skills.extend(profile.excluded_skills.iter().cloned());
    }
    excluded_tools.sort();
    excluded_tools.dedup();
    excluded_skills.sort();
    excluded_skills.dedup();
    let mut layer = toml::map::Map::new();
    if let Some(profile) = profile {
        let profile_value = toml::Value::try_from(profile).map_err(|error| {
            FunctionCallError::RespondToModel(format!("invalid Flowdex tool profile: {error}"))
        })?;
        let TomlValue::Table(mut values) = profile_value else {
            unreachable!("Flowdex tool profiles serialize as tables")
        };
        values.remove("excluded_tools");
        values.remove("excluded_skills");
        for (key, value) in values {
            layer.insert(key, value);
        }
    }
    if !excluded_skills.is_empty() {
        let entries = excluded_skills
            .iter()
            .map(|name| {
                TomlValue::Table(toml::map::Map::from_iter([
                    ("name".into(), TomlValue::String(name.clone())),
                    ("enabled".into(), TomlValue::Boolean(false)),
                ]))
            })
            .collect();
        layer.insert(
            "skills".into(),
            TomlValue::Table(toml::map::Map::from_iter([(
                "config".into(),
                TomlValue::Array(entries),
            )])),
        );
    }
    if layer.is_empty() {
        config.flowdex_config.active_agent_excluded_tools = excluded_tools;
        return Ok(());
    }
    let mut layers = config
        .config_layer_stack
        .layers_low_to_high()
        .cloned()
        .collect::<Vec<_>>();
    layers.push(ConfigLayerEntry::new(
        ConfigLayerSource::SessionFlags,
        TomlValue::Table(layer),
    ));
    let stack = ConfigLayerStack::new(
        layers,
        config.config_layer_stack.requirements().clone(),
        config.config_layer_stack.requirements_toml().clone(),
    )
    .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    let cfg =
        deserialize_config_toml_with_base(stack.effective_config(), config.codex_home.as_path())
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    let next = Config::load_config_with_layer_stack(
        LOCAL_FS.as_ref(),
        cfg,
        ConfigOverrides {
            cwd: Some(config.cwd.to_path_buf()),
            model: config.model.clone(),
            model_provider: Some(config.model_provider_id.clone()),
            service_tier: Some(config.service_tier.clone()),
            ..Default::default()
        },
        config.codex_home.clone(),
        stack,
    )
    .await
    .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    config.config_layer_stack = next.config_layer_stack;
    config.mcp_servers = next.mcp_servers;
    config.web_search_mode = next.web_search_mode;
    config.experimental_request_user_input_enabled = next.experimental_request_user_input_enabled;
    config.update_plan_enabled = next.update_plan_enabled;
    config.include_skill_instructions = next.include_skill_instructions;
    config.skill_max_context_tokens = next.skill_max_context_tokens;
    config.flowdex_config.active_agent_excluded_tools = excluded_tools;
    Ok(())
}

async fn handle_spawn(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        step_context,
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
    apply_spawn_agent_role(&session, &mut config, profile).await?;
    apply_flowdex_tool_profile(
        &session,
        &mut config,
        args.tool_profile
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .await?;
    // Profile loading rebuilds Config from persisted layers. Restore the live turn's
    // approval and sandbox selection before dispatching the child.
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
    apply_explicit_spawn_agent_model_overrides(
        &session,
        turn.as_ref(),
        &mut config,
        args.model.as_deref(),
        args.reasoning_effort,
    )
    .await?;
    let spawn_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        profile,
        Some(name.to_string()),
        Some(name.to_string()),
    )?;
    let child_path = spawn_source.get_agent_path().ok_or_else(|| {
        FunctionCallError::RespondToModel("spawned agent is missing an agent path".into())
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = InterAgentCommunication::new(
        author,
        child_path,
        Vec::new(),
        instructions.to_string(),
        true,
    );
    let context = AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id);
    let ui_event = begin_spawn_ui_event(&session, &turn, instructions.to_string()).await;
    let child = match session
        .services
        .agent_control
        .spawn_agent_with_communication(
            config,
            communication,
            context,
            Some(spawn_source),
            SpawnAgentOptions {
                parent_thread_id: Some(session.thread_id),
                parent_turn_id: Some(turn.sub_id.clone()),
                root_turn_id: turn.turn_metadata_state.root_turn_id(),
                cyber_access_program: turn.cyber_access_program,
                environments: Some(step_context.environments.to_selections()),
                completion_delivery: SpawnAgentCompletionDelivery::StatusOnly,
                ..Default::default()
            },
        )
        .await
    {
        Ok(child) => child,
        Err(error) => {
            finish_spawn_ui_event(
                &session,
                &turn,
                ui_event,
                None,
                None,
                None,
                AgentStatus::NotFound,
            )
            .await;
            return Err(collab_spawn_error(error));
        }
    };
    finish_spawn_ui_event(
        &session,
        &turn,
        ui_event,
        Some(child.thread_id),
        child.metadata.agent_nickname.clone(),
        child.metadata.agent_role.clone(),
        child.status.clone(),
    )
    .await;
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
    if trigger_turn && super::task::task_associated_agent(target).is_some() {
        return Err(FunctionCallError::RespondToModel(
            "task-associated agents must be resumed with resumeAgent; trigger-turn delivery is not supported".into(),
        ));
    }
    if trigger_turn {
        refresh_agent_runtime_settings(&session, &turn, target).await?;
    }
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
    let communication =
        InterAgentCommunication::new(author, receiver, Vec::new(), args.message, trigger_turn);
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
        .send_inter_agent_communication(
            target,
            communication,
            context,
            crate::TurnStartOptions {
                parent_turn_id: Some(turn.sub_id.clone()),
                root_turn_id: turn.turn_metadata_state.root_turn_id(),
                cyber_access_program: turn.cyber_access_program,
                ..Default::default()
            },
        )
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
    let prior_status = session.services.agent_control.get_status(id).await;
    if matches!(prior_status, AgentStatus::NotFound) {
        return Ok(JsonOutput::new(
            serde_json::json!({"agentId": id.to_string(), "status": "notFound"}),
        ));
    }
    if !is_final(&prior_status) {
        return Err(FunctionCallError::RespondToModel(
            "agent must have a completed prior turn".into(),
        ));
    }

    let task_id = super::task::task_associated_agent(id);
    let _task_gate = match task_id.as_deref() {
        Some(task_id) => Some(super::task::acquire_task_gate(task_id).await),
        None => None,
    };
    refresh_agent_runtime_settings(&session, &turn, id).await?;

    if mode == "compact" {
        let operation = session
            .services
            .agent_control
            .submit_compaction(id)
            .await
            .map_err(|err| collab_agent_error(id, err))?;
        let compact_status = session
            .services
            .agent_control
            .wait_for_submitted_operation(operation)
            .await;
        if !matches!(compact_status, AgentStatus::Completed(_)) {
            return Ok(JsonOutput::new(status_value(id, compact_status)));
        }
        let operation =
            submit_attributed_trigger_turn(&session, &turn, id, instructions, task_id.as_deref())
                .await?;
        let submission_id = operation.submission_id().to_string();
        let status = session
            .services
            .agent_control
            .wait_for_submitted_operation(operation)
            .await;
        if let Some(task_id) = task_id.as_deref() {
            super::task::finish_task_operation(
                &session,
                &turn,
                task_id,
                &submission_id,
                terminal_state(&status),
                true,
            )
            .await?;
        }
        return Ok(JsonOutput::new(status_value(id, status)));
    }

    if mode == "handoff" {
        let handoff_operation =
            submit_attributed_trigger_turn(&session, &turn, id, HANDOFF_PROMPT, task_id.as_deref())
                .await?;
        let handoff_submission_id = handoff_operation.submission_id().to_string();
        let handoff_status = session
            .services
            .agent_control
            .wait_for_submitted_operation(handoff_operation)
            .await;
        if let Some(task_id) = task_id.as_deref() {
            super::task::finish_task_operation(
                &session,
                &turn,
                task_id,
                &handoff_submission_id,
                terminal_state(&handoff_status),
                false,
            )
            .await?;
        }
        let handoff_text = match handoff_status {
            AgentStatus::Completed(Some(text)) if !text.trim().is_empty() => text,
            status => return Ok(JsonOutput::new(status_value(id, status))),
        };
        let config = session
            .services
            .agent_control
            .get_agent_config(id)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel("agent configuration unavailable".into())
            })?
            .as_ref()
            .clone();
        let environments = session
            .services
            .agent_control
            .get_agent_config_snapshot(id)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel("agent configuration unavailable".into())
            })?
            .environment_selections()
            .to_vec();
        let parent_id = session.thread_id;
        let depth = next_thread_spawn_depth(&turn.session_source);
        let replacement_name = format!("handoff_{}", ThreadId::new().to_string().replace('-', ""));
        let replacement_nickname = session
            .services
            .agent_control
            .get_agent_metadata(id)
            .and_then(|metadata| metadata.agent_nickname);
        let source = thread_spawn_source(
            parent_id,
            &turn.session_source,
            depth,
            None,
            Some(replacement_name),
            replacement_nickname,
        )?;
        let prompt = format!(
            "Handoff:\n{}\n\nInstructions:\n{}",
            truncate_message(&handoff_text),
            instructions
        );
        let replacement_model = config.model.clone().unwrap_or_default();
        let replacement_reservation = match task_id.as_deref() {
            Some(task_id) => Some(
                super::task::reserve_task_operation(&session, &turn, task_id, &replacement_model)
                    .await?,
            ),
            None => None,
        };
        let ui_event = begin_spawn_ui_event(&session, &turn, prompt.clone()).await;
        let child = match session
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
                    parent_turn_id: Some(turn.sub_id.clone()),
                    root_turn_id: turn.turn_metadata_state.root_turn_id(),
                    cyber_access_program: turn.cyber_access_program,
                    environments: Some(environments),
                    completion_delivery: SpawnAgentCompletionDelivery::StatusOnly,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(child) => child,
            Err(error) => {
                finish_spawn_ui_event(
                    &session,
                    &turn,
                    ui_event,
                    None,
                    None,
                    None,
                    AgentStatus::NotFound,
                )
                .await;
                if let (Some(task_id), Some(reservation_id)) =
                    (task_id.as_deref(), replacement_reservation.as_deref())
                {
                    super::task::cancel_task_operation_reservation(
                        &session,
                        &turn,
                        task_id,
                        reservation_id,
                    )
                    .await?;
                }
                return Err(collab_spawn_error(error));
            }
        };
        finish_spawn_ui_event(
            &session,
            &turn,
            ui_event,
            Some(child.thread_id),
            child.metadata.agent_nickname.clone(),
            child.metadata.agent_role.clone(),
            child.status.clone(),
        )
        .await;
        let replacement_id = child.thread_id;
        if let (Some(task_id), Some(reservation_id)) =
            (task_id.as_deref(), replacement_reservation.as_deref())
        {
            super::task::associate_task_agent(replacement_id, task_id);
            super::task::bind_task_operation(
                &session,
                &turn,
                task_id,
                reservation_id,
                &child.initial_submission_id,
                &replacement_id.to_string(),
            )
            .await?;
        }
        let replacement_status = match child.initial_operation {
            Some(operation) => {
                session
                    .services
                    .agent_control
                    .wait_for_submitted_operation(operation)
                    .await
            }
            None => wait_for_terminal(&session, replacement_id).await,
        };
        if let Some(task_id) = super::task::task_associated_agent(replacement_id) {
            super::task::finish_task_operation(
                &session,
                &turn,
                &task_id,
                &child.initial_submission_id,
                terminal_state(&replacement_status),
                true,
            )
            .await?;
        }
        return Ok(JsonOutput::new(status_value(
            replacement_id,
            replacement_status,
        )));
    }

    let operation =
        submit_attributed_trigger_turn(&session, &turn, id, instructions, task_id.as_deref())
            .await?;
    let submission_id = operation.submission_id().to_string();
    let status = session
        .services
        .agent_control
        .wait_for_submitted_operation(operation)
        .await;
    if let Some(task_id) = task_id.as_deref() {
        super::task::finish_task_operation(
            &session,
            &turn,
            task_id,
            &submission_id,
            terminal_state(&status),
            true,
        )
        .await?;
    }
    Ok(JsonOutput::new(status_value(id, status)))
}

async fn refresh_agent_runtime_settings(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    agent_id: ThreadId,
) -> Result<(), FunctionCallError> {
    session
        .services
        .agent_control
        .update_agent_settings(
            agent_id,
            SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    approval_policy: Some(turn.approval_policy()),
                    approvals_reviewer: Some(turn.initial_settings.approvals_reviewer()),
                    ..Default::default()
                },
                sandbox_policy: Some(turn.sandbox_policy()),
                windows_sandbox_level: Some(turn.windows_sandbox_level),
                ..Default::default()
            },
        )
        .await
        .map_err(|err| collab_agent_error(agent_id, err))
}

async fn submit_trigger_turn(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    id: ThreadId,
    message: &str,
) -> Result<crate::agent::control::SubmittedAgentOperation, FunctionCallError> {
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
    let communication =
        InterAgentCommunication::new(author, receiver, Vec::new(), message.to_string(), true);
    let context =
        AgentCommunicationContext::new(AgentCommunicationKind::Followup, session.thread_id);
    let operation = session
        .services
        .agent_control
        .submit_inter_agent_communication_operation(
            id,
            communication,
            context,
            crate::TurnStartOptions {
                parent_turn_id: Some(turn.sub_id.clone()),
                root_turn_id: turn.turn_metadata_state.root_turn_id(),
                cyber_access_program: turn.cyber_access_program,
                ..Default::default()
            },
        )
        .await
        .map_err(|err| collab_agent_error(id, err))?;
    Ok(operation)
}

async fn submit_attributed_trigger_turn(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    id: ThreadId,
    message: &str,
    task_id: Option<&str>,
) -> Result<crate::agent::control::SubmittedAgentOperation, FunctionCallError> {
    let model = session
        .services
        .agent_control
        .get_agent_config(id)
        .await
        .and_then(|config| config.model.clone())
        .unwrap_or_default();
    let reservation_id = match task_id {
        Some(task_id) => {
            Some(super::task::reserve_task_operation(session, turn, task_id, &model).await?)
        }
        None => None,
    };
    let operation = match submit_trigger_turn(session, turn, id, message).await {
        Ok(operation) => operation,
        Err(error) => {
            if let (Some(task_id), Some(reservation_id)) = (task_id, reservation_id.as_deref()) {
                super::task::cancel_task_operation_reservation(
                    session,
                    turn,
                    task_id,
                    reservation_id,
                )
                .await?;
            }
            return Err(error);
        }
    };
    if let (Some(task_id), Some(reservation_id)) = (task_id, reservation_id.as_deref()) {
        super::task::bind_task_operation(
            session,
            turn,
            task_id,
            reservation_id,
            operation.submission_id(),
            &id.to_string(),
        )
        .await?;
    }
    Ok(operation)
}

fn terminal_state(status: &AgentStatus) -> &'static str {
    if matches!(status, AgentStatus::Completed(_)) {
        "completed"
    } else {
        "errored"
    }
}

pub(crate) async fn wait_for_terminal(
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
        Err(error) if matches!(error.details(), CodexErrorDetails::ThreadNotFound(_)) => {
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

pub(crate) fn truncate_message(message: &str) -> String {
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
    fn log_output(&self) -> String {
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
