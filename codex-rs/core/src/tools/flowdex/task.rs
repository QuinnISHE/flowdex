use super::agents::status_value;
use super::agents::truncate_message;
use super::verification::FlowdexVerifyHandler;
use crate::agent::AgentStatus;
use crate::agent::control::SpawnAgentCompletionDelivery;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::apply_explicit_spawn_agent_model_overrides;
use crate::tools::handlers::multi_agents_common::apply_spawn_agent_role;
use crate::tools::handlers::multi_agents_common::apply_spawn_agent_runtime_overrides;
use crate::tools::handlers::multi_agents_common::build_agent_spawn_config;
use crate::tools::handlers::multi_agents_common::collab_spawn_error;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_flowdex::FlowdexStore;
use codex_flowdex::FlowdexStoreError;
use codex_flowdex::RunInfo;
use codex_flowdex::TaskDeclaration;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::InterAgentCommunication;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::shell_command_backend_for_features;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use tokio::sync::OwnedMutexGuard;

const CREATE: &str = "flowdex_create_task";
const RUN: &str = "flowdex_task_run_agent";
const VERIFY: &str = "flowdex_task_verify";
const INTEGRATE: &str = "flowdex_task_integrate";

static ASSOCIATIONS: OnceLock<Mutex<HashMap<ThreadId, String>>> = OnceLock::new();
static GATES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();

fn associations() -> &'static Mutex<HashMap<ThreadId, String>> {
    ASSOCIATIONS.get_or_init(Default::default)
}
fn gates() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    GATES.get_or_init(Default::default)
}
fn task_for_agent(id: ThreadId) -> Option<String> {
    associations().lock().ok()?.get(&id).cloned()
}
pub(crate) fn associated_task(id: ThreadId) -> Option<String> {
    task_for_agent(id)
}
fn associate(id: ThreadId, task: &str) {
    if let Ok(mut map) = associations().lock() {
        map.insert(id, task.to_string());
    }
}
pub(crate) fn agents_for_task(task: &str) -> Vec<ThreadId> {
    associations()
        .lock()
        .map(|map| {
            map.iter()
                .filter_map(|(id, associated)| (associated == task).then_some(*id))
                .collect()
        })
        .unwrap_or_default()
}
pub(crate) fn forget_task_agents(task: &str) {
    if let Ok(mut map) = associations().lock() {
        map.retain(|_, associated| associated != task);
    }
}
async fn task_gate(task: &str) -> OwnedMutexGuard<()> {
    let gate = gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(task.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    gate.lock_owned().await
}
pub(crate) async fn acquire_task_gate(task: &str) -> OwnedMutexGuard<()> {
    task_gate(task).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    name: String,
    instructions: String,
    #[serde(default)]
    read_scope: Vec<String>,
    #[serde(default)]
    write_scope: Vec<String>,
    #[serde(default)]
    verification: Vec<String>,
    workflow_path: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSpec {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) display_name: Option<String>,
    pub(crate) instructions: String,
    pub(crate) profile: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) tool_profile: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    task_id: String,
    agent: AgentSpec,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskIdArgs {
    task_id: String,
}

pub(crate) struct FlowdexCreateTaskHandler;
pub(crate) struct FlowdexTaskRunAgentHandler;
pub(crate) struct FlowdexTaskVerifyHandler;
pub(crate) struct FlowdexTaskIntegrateHandler;

macro_rules! executor {
    ($ty:ty, $name:expr, $spec:ident, $handler:ident) => {
        impl ToolExecutor<ToolInvocation> for $ty {
            fn tool_name(&self) -> ToolName {
                ToolName::plain($name)
            }
            fn spec(&self) -> ToolSpec {
                $spec()
            }
            fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
                Box::pin(async move { $handler(invocation).await.map(boxed_tool_output) })
            }
        }
        impl CoreToolRuntime for $ty {}
    };
}
executor!(FlowdexCreateTaskHandler, CREATE, create_spec, handle_create);
executor!(FlowdexTaskRunAgentHandler, RUN, run_spec, handle_run);
impl ToolExecutor<ToolInvocation> for FlowdexTaskVerifyHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(VERIFY)
    }
    fn spec(&self) -> ToolSpec {
        task_verify_spec()
    }
    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_verify(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for FlowdexTaskVerifyHandler {
    fn waits_for_runtime_cancellation(&self) -> bool {
        true
    }
}
executor!(
    FlowdexTaskIntegrateHandler,
    INTEGRATE,
    task_integrate_spec,
    handle_integrate
);

fn object_spec(
    name: &str,
    description: &str,
    properties: BTreeMap<String, JsonSchema>,
    required: Vec<String>,
) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.into(),
        description: description.into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(required), Some(false.into())),
        output_schema: Some(serde_json::json!({"type":"object"})),
    })
}
fn create_spec() -> ToolSpec {
    object_spec(
        CREATE,
        "Create a Flowdex task.",
        BTreeMap::from([
            (String::from("name"), JsonSchema::string(None)),
            (String::from("instructions"), JsonSchema::string(None)),
            (
                String::from("read_scope"),
                JsonSchema::array(JsonSchema::string(None), None),
            ),
            (
                String::from("write_scope"),
                JsonSchema::array(JsonSchema::string(None), None),
            ),
            (
                String::from("verification"),
                JsonSchema::array(JsonSchema::string(None), None),
            ),
            (String::from("workflow_path"), JsonSchema::string(None)),
        ]),
        vec!["name".into(), "instructions".into(), "workflow_path".into()],
    )
}
fn run_spec() -> ToolSpec {
    object_spec(
        RUN,
        "Run an agent in a Flowdex task worktree.",
        BTreeMap::from([
            (String::from("task_id"), JsonSchema::string(None)),
            (String::from("agent"), JsonSchema::default()),
        ]),
        vec!["task_id".into(), "agent".into()],
    )
}
fn task_verify_spec() -> ToolSpec {
    object_spec(
        VERIFY,
        "Verify a Flowdex task worktree.",
        BTreeMap::from([(String::from("task_id"), JsonSchema::string(None))]),
        vec!["task_id".into()],
    )
}
fn task_integrate_spec() -> ToolSpec {
    object_spec(
        INTEGRATE,
        "Integrate a Flowdex task.",
        BTreeMap::from([(String::from("task_id"), JsonSchema::string(None))]),
        vec!["task_id".into()],
    )
}

fn parse(payload: ToolPayload, message: &str) -> Result<String, FunctionCallError> {
    match payload {
        ToolPayload::Function { arguments } => Ok(arguments),
        _ => Err(FunctionCallError::RespondToModel(message.into())),
    }
}
pub(crate) fn runtime_id(invocation: &ToolInvocation) -> Result<String, FunctionCallError> {
    match &invocation.source {
        crate::tools::context::ToolCallSource::CodeMode { cell_id, .. } => {
            Ok(super::workflow_store_run_id(cell_id))
        }
        _ => Err(FunctionCallError::RespondToModel(
            "Flowdex task tools are available only inside a workflow".into(),
        )),
    }
}
pub(crate) async fn task_store(
    invocation: &ToolInvocation,
) -> Result<(FlowdexStore, PathBuf, String), FunctionCallError> {
    open_store(&invocation.session, &invocation.turn).await
}

pub(crate) async fn open_store(
    _session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
) -> Result<(FlowdexStore, PathBuf, String), FunctionCallError> {
    if !turn.config.active_project.is_trusted() {
        return Err(FunctionCallError::RespondToModel(
            "Flowdex tasks require a trusted Git repository".into(),
        ));
    }
    let cwd = turn
        .environments
        .single_local_environment_cwd()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| {
            FunctionCallError::RespondToModel("Flowdex tasks require one local environment".into())
        })?;
    let home = turn.config.codex_home.to_path_buf();
    let identity = tokio::task::spawn_blocking({
        let cwd = cwd.clone();
        move || repository_identity(&cwd)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(FunctionCallError::RespondToModel)?;
    let repository_identity = identity.clone();
    let store_cwd = cwd.clone();
    let store = tokio::task::spawn_blocking(move || {
        FlowdexStore::open(&home, repository_identity, &store_cwd)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    Ok((store, cwd, identity))
}

pub(crate) async fn open_existing_store(
    _session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
) -> Result<(Option<FlowdexStore>, PathBuf), FunctionCallError> {
    if !turn.config.active_project.is_trusted() {
        return Err(FunctionCallError::RespondToModel(
            "Flowdex tasks require a trusted Git repository".into(),
        ));
    }
    let cwd = turn
        .environments
        .single_local_environment_cwd()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| {
            FunctionCallError::RespondToModel("Flowdex tasks require one local environment".into())
        })?;
    let home = turn.config.codex_home.to_path_buf();
    let (identity, repository_root) = tokio::task::spawn_blocking({
        let cwd = cwd.clone();
        move || Ok::<_, String>((repository_identity(&cwd)?, repository_root(&cwd)?))
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(FunctionCallError::RespondToModel)?;
    let integration_worktree = cwd;
    let store = tokio::task::spawn_blocking(move || {
        FlowdexStore::open_existing(&home, identity, &integration_worktree)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    Ok((store, repository_root))
}

fn repository_identity(cwd: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("current environment is not a Git repository".to_string());
    }
    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if common_dir.is_empty() {
        return Err("current environment is not a Git repository".to_string());
    }
    std::fs::canonicalize(common_dir)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

fn repository_root(cwd: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("current environment is not a Git repository".to_string());
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Err("current environment is not a Git repository".to_string());
    }
    std::fs::canonicalize(root).map_err(|e| e.to_string())
}

pub(crate) async fn reserve_task_operation(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    task_id: &str,
    model: &str,
) -> Result<String, FunctionCallError> {
    let (store, _, _) = open_store(session, turn).await?;
    let task_id = task_id.to_string();
    let model = model.to_string();
    tokio::task::spawn_blocking(move || store.reserve_operation(&task_id, &model))
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(task_error)
}

pub(crate) async fn bind_task_operation(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    task_id: &str,
    reservation_id: &str,
    operation_id: &str,
    agent_id: &str,
) -> Result<(), FunctionCallError> {
    let (store, _, _) = open_store(session, turn).await?;
    let task_id = task_id.to_string();
    let reservation_id = reservation_id.to_string();
    let operation_id = operation_id.to_string();
    let agent_id = agent_id.to_string();
    tokio::task::spawn_blocking(move || {
        store.bind_operation(&task_id, &reservation_id, &operation_id, &agent_id)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    Ok(())
}

pub(crate) async fn cancel_task_operation_reservation(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    task_id: &str,
    reservation_id: &str,
) -> Result<(), FunctionCallError> {
    let (store, _, _) = open_store(session, turn).await?;
    let task_id = task_id.to_string();
    let reservation_id = reservation_id.to_string();
    tokio::task::spawn_blocking(move || {
        store.cancel_operation_reservation(&task_id, &reservation_id)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)
}

pub(crate) async fn finish_task_operation(
    session: &std::sync::Arc<crate::session::session::Session>,
    turn: &std::sync::Arc<crate::session::turn_context::TurnContext>,
    task_id: &str,
    operation_id: &str,
    terminal: &str,
    commit_changes: bool,
) -> Result<(), FunctionCallError> {
    let (store, _, _) = open_store(session, turn).await?;
    let task_id = task_id.to_string();
    let operation_id = operation_id.to_string();
    let terminal = terminal.to_string();
    tokio::task::spawn_blocking(move || {
        if terminal == "completed" && commit_changes {
            let task = store.task(&task_id)?;
            store.commit_operation_changes(
                &task_id,
                &operation_id,
                &format!("flowdex: {}", task.name.trim()),
            )?;
        }
        store.finish_operation(&task_id, &operation_id, &terminal)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    Ok(())
}
fn task_error(error: FlowdexStoreError) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("Flowdex task failed: {error}"))
}

async fn handle_create(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let args: CreateArgs = serde_json::from_str(&parse(
        invocation.payload.clone(),
        "flowdex createTask expects JSON arguments",
    )?)
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    if args.name.trim().is_empty() || args.instructions.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "name and instructions must be non-empty".into(),
        ));
    }
    for values in [&args.read_scope, &args.write_scope, &args.verification] {
        if values.iter().any(|v| v.trim().is_empty()) {
            return Err(FunctionCallError::RespondToModel(
                "task arrays must contain only non-empty strings".into(),
            ));
        }
    }
    let run_id = runtime_id(&invocation)?;
    let (store, cwd, root) = task_store(&invocation).await?;
    let declaration = TaskDeclaration {
        id: uuid::Uuid::new_v4().to_string(),
        name: args.name.trim().into(),
        instructions: args.instructions.trim().into(),
        read_scope: args.read_scope,
        write_scope: args.write_scope,
        verification: args.verification,
    };
    let run = RunInfo {
        run_id,
        parent_thread_id: invocation.session.thread_id.to_string(),
        workflow_path: args.workflow_path,
        parent_run_id: None,
        workflow_identity: None,
        repository_identity: root,
        integration_worktree: cwd,
    };
    let task = tokio::task::spawn_blocking(move || store.create_task(&run, &declaration))
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(task_error)?;
    Ok(JsonOutput(serde_json::json!({"taskId": task.id})))
}

async fn handle_run(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    handle_run_with_review(invocation, None).await
}

pub(crate) struct ReviewDispatch {
    pub(crate) operation: codex_flowdex::store::ReviewOperation,
    pub(crate) store: std::sync::Arc<codex_flowdex::store::FlowdexStore>,
    pub(crate) worktree: Option<std::path::PathBuf>,
}

struct ReviewRegistrationGuard(Option<AgentPath>);

impl Drop for ReviewRegistrationGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            super::review::deactivate_review_agent(&path);
        }
    }
}

pub(crate) async fn handle_run_with_review(
    invocation: ToolInvocation,
    review: Option<ReviewDispatch>,
) -> Result<JsonOutput, FunctionCallError> {
    let args: RunArgs = serde_json::from_str(&parse(
        invocation.payload.clone(),
        "flowdex task.runAgent expects JSON arguments",
    )?)
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let name = args.agent.name.trim();
    let supplied = args.agent.instructions.trim();
    if name.is_empty() || supplied.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "name and instructions must be non-empty".into(),
        ));
    }
    if args
        .agent
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_none()
        && args.agent.model.is_none()
        && args.agent.reasoning_effort.is_none()
    {
        return Err(FunctionCallError::RespondToModel(
            "runAgent requires profile, model, or reasoningEffort".into(),
        ));
    }
    let (store, _cwd, _) = task_store(&invocation).await?;
    let task = tokio::task::spawn_blocking({
        let id = args.task_id.clone();
        move || store.task(&id)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    let _gate = task_gate(&task.id).await;
    let mut config = build_agent_spawn_config(
        &invocation.session.get_base_instructions().await,
        invocation.turn.as_ref(),
        invocation.step_context.environments.primary(),
    )?;
    apply_spawn_agent_role(
        &invocation.session,
        &mut config,
        args.agent
            .profile
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .await?;
    super::agents::apply_flowdex_tool_profile(
        &invocation.session,
        &mut config,
        args.agent
            .tool_profile
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .await?;
    // Profile loading rebuilds Config from persisted layers. Restore the live turn's
    // approval and sandbox selection before rebasing it onto the task worktree.
    apply_spawn_agent_runtime_overrides(
        &mut config,
        invocation.turn.as_ref(),
        invocation.step_context.environments.primary(),
    )?;
    apply_explicit_spawn_agent_model_overrides(
        &invocation.session,
        invocation.turn.as_ref(),
        &mut config,
        args.agent.model.as_deref(),
        args.agent.reasoning_effort,
    )
    .await?;
    let execution_worktree = review
        .as_ref()
        .and_then(|dispatch| dispatch.worktree.as_deref())
        .unwrap_or(&task.worktree_path);
    let task_cwd = AbsolutePathBuf::from_absolute_path(execution_worktree)
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    config.cwd = task_cwd.clone();
    config.workspace_roots = vec![task_cwd.clone()];
    config
        .permissions
        .set_legacy_sandbox_policy(invocation.turn.sandbox_policy(), task_cwd.as_path())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("permission_profile is invalid: {err}"))
        })?;
    let mut environments = invocation.step_context.environments.to_selections();
    for environment in &mut environments {
        environment.cwd = PathUri::from_abs_path(&task_cwd);
        environment.workspace_roots = vec![PathUri::from_abs_path(&task_cwd)];
    }
    let depth = next_thread_spawn_depth(&invocation.turn.session_source);
    if exceeds_thread_spawn_depth_limit(depth, invocation.turn.config.agent_max_depth) {
        return Err(FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".into(),
        ));
    }
    let source = thread_spawn_source(
        invocation.session.thread_id,
        &invocation.turn.session_source,
        depth,
        args.agent.profile.as_deref(),
        Some(name.to_string()),
        args.agent.display_name.clone(),
    )?;
    let child_path = source.get_agent_path().ok_or_else(|| {
        FunctionCallError::RespondToModel("spawned agent is missing an agent path".into())
    })?;
    let _review_guard = if let Some(review) = review.as_ref() {
        super::review::activate_review_agent(
            child_path.clone(),
            review.operation.clone(),
            Arc::clone(&review.store),
        );
        Some(ReviewRegistrationGuard(Some(child_path.clone())))
    } else {
        None
    };
    let author = invocation
        .turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let instructions = task_agent_instructions(
        &task.instructions,
        &task.read_scope,
        &task.write_scope,
        &task.verification,
        supplied,
        review.is_some(),
    );
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| invocation.turn.model_info.slug.clone());
    let reservation_id = if review.is_none() {
        Some(reserve_task_operation(&invocation.session, &invocation.turn, &task.id, &model).await?)
    } else {
        None
    };
    let ui_event = super::agents::begin_spawn_ui_event(
        &invocation.session,
        &invocation.turn,
        instructions.clone(),
    )
    .await;
    let child = match invocation
        .session
        .services
        .agent_control
        .spawn_agent_with_communication(
            config.clone(),
            InterAgentCommunication::new(
                author,
                child_path.clone(),
                Vec::new(),
                instructions,
                true,
            ),
            AgentCommunicationContext::new(
                AgentCommunicationKind::Spawn,
                invocation.session.thread_id,
            ),
            Some(source),
            SpawnAgentOptions {
                parent_thread_id: Some(invocation.session.thread_id),
                environments: Some(environments),
                completion_delivery: SpawnAgentCompletionDelivery::StatusOnly,
                ..Default::default()
            },
        )
        .await
    {
        Ok(child) => child,
        Err(error) => {
            super::agents::finish_spawn_ui_event(
                &invocation.session,
                &invocation.turn,
                ui_event,
                None,
                None,
                None,
                AgentStatus::NotFound,
            )
            .await;
            if let Some(reservation_id) = reservation_id.as_deref() {
                cancel_task_operation_reservation(
                    &invocation.session,
                    &invocation.turn,
                    &task.id,
                    reservation_id,
                )
                .await?;
            }
            return Err(collab_spawn_error(error));
        }
    };
    super::agents::finish_spawn_ui_event(
        &invocation.session,
        &invocation.turn,
        ui_event,
        Some(child.thread_id),
        child.metadata.agent_nickname.clone(),
        child.metadata.agent_role.clone(),
        child.status.clone(),
    )
    .await;
    let id = child.thread_id;
    let operation_id = child.initial_submission_id.clone();
    if let Some(reservation_id) = reservation_id.as_deref() {
        associate(id, &task.id);
        bind_task_operation(
            &invocation.session,
            &invocation.turn,
            &task.id,
            reservation_id,
            &operation_id,
            &id.to_string(),
        )
        .await?;
    }
    let status = match child.initial_operation {
        Some(operation) => {
            tokio::select! {
                _ = invocation.cancellation_token.cancelled() => {
                    let _ = invocation
                        .session
                        .services
                        .agent_control
                        .interrupt_agent(id)
                        .await;
                    if reservation_id.is_some() {
                        let _ = finish_task_operation(
                            &invocation.session,
                            &invocation.turn,
                            &task.id,
                            &operation_id,
                            "cancelled",
                            false,
                        )
                        .await;
                    }
                    return Err(FunctionCallError::RespondToModel(
                        "Flowdex task cancelled".to_string(),
                    ));
                }
                status = invocation
                    .session
                    .services
                    .agent_control
                    .wait_for_submitted_operation(operation) => status,
            }
        }
        None => {
            tokio::select! {
                _ = invocation.cancellation_token.cancelled() => {
                    let _ = invocation
                        .session
                        .services
                        .agent_control
                        .interrupt_agent(id)
                        .await;
                    if reservation_id.is_some() {
                        let _ = finish_task_operation(
                            &invocation.session,
                            &invocation.turn,
                            &task.id,
                            &operation_id,
                            "cancelled",
                            false,
                        )
                        .await;
                    }
                    return Err(FunctionCallError::RespondToModel(
                        "Flowdex task cancelled".to_string(),
                    ));
                }
                status = super::agents::wait_for_terminal(&invocation.session, id) => status,
            }
        }
    };
    let terminal = if matches!(status, crate::agent::AgentStatus::Completed(_)) {
        "completed"
    } else {
        "errored"
    };
    if reservation_id.is_some() {
        finish_task_operation(
            &invocation.session,
            &invocation.turn,
            &task.id,
            &operation_id,
            terminal,
            true,
        )
        .await?;
    }
    Ok(JsonOutput(status_value(id, status)))
}

fn task_agent_instructions(
    task_instructions: &str,
    read_scope: &[String],
    write_scope: &[String],
    verification: &[String],
    supplied: &str,
    is_review: bool,
) -> String {
    if is_review {
        return supplied.to_string();
    }
    let verification = if verification.is_empty() {
        "Flowdex verification: no task commands declared.".to_string()
    } else {
        format!(
            "Flowdex will run these task verification commands automatically after you finish:\n{}\nDo not run them yourself solely to complete workflow verification; Flowdex owns that step.",
            verification
                .iter()
                .map(|command| format!("- {command}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "Task requirements:\n{task_instructions}\n\nRead scope (advisory): {read_scope:?}\nWrite scope (advisory): {write_scope:?}\n\n{verification}\n\n{supplied}\n\nFinish with a brief useful summary. Flowdex commits successful task changes automatically."
    )
}

pub(crate) async fn run_task_agent_with_review(
    mut invocation: ToolInvocation,
    task_id: String,
    agent: AgentSpec,
    review: ReviewDispatch,
) -> Result<JsonOutput, FunctionCallError> {
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({
            "task_id": task_id,
            "agent": {
                "name": agent.name,
                "display_name": agent.display_name,
                "instructions": agent.instructions,
                "profile": agent.profile,
                "tool_profile": agent.tool_profile,
                "model": agent.model,
                "reasoning_effort": agent.reasoning_effort,
            }
        })
        .to_string(),
    };
    handle_run_with_review(invocation, Some(review)).await
}

async fn handle_verify(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    let args: TaskIdArgs = serde_json::from_str(&parse(
        invocation.payload.clone(),
        "flowdex task.verify expects JSON arguments",
    )?)
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let _gate = task_gate(&args.task_id).await;
    let (store, repository_root, base_identity) = task_store(&invocation).await?;
    let task_id = args.task_id.clone();
    let task = tokio::task::spawn_blocking({
        let id = task_id.clone();
        move || store.task(&id)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    if task.verification.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "task has no verification commands".into(),
        ));
    }
    let value = serde_json::json!({"commands": task.verification});
    let payload = ToolPayload::Function {
        arguments: value.to_string(),
    };
    let mut task_invocation = invocation.clone();
    task_invocation.payload = payload;
    let task_cwd = AbsolutePathBuf::from_absolute_path(&task.worktree_path)
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    if invocation.cancellation_token.is_cancelled() {
        return Err(FunctionCallError::RespondToModel(
            "verification cancelled".to_string(),
        ));
    }
    let worktree_path = task.worktree_path.clone();
    let worktree_identity =
        tokio::task::spawn_blocking(move || repository_identity(&worktree_path))
            .await
            .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
            .map_err(FunctionCallError::RespondToModel)?;
    if worktree_identity != base_identity {
        return Err(FunctionCallError::RespondToModel(
            "task worktree is not part of the trusted repository".to_string(),
        ));
    }
    if invocation.cancellation_token.is_cancelled() {
        return Err(FunctionCallError::RespondToModel(
            "verification cancelled".to_string(),
        ));
    }
    let mut config = (*task_invocation.turn.config).clone();
    config.cwd = task_cwd.clone();
    config.workspace_roots = vec![task_cwd.clone()];
    config
        .permissions
        .set_workspace_roots(vec![task_cwd.clone()]);
    task_invocation.turn = std::sync::Arc::new(
        task_invocation
            .turn
            .clone_with_config(std::sync::Arc::new(config)),
    );
    let verifier = FlowdexVerifyHandler::new(shell_command_backend_for_features(
        invocation.turn.config.features.get(),
    ));
    let output = verifier
        .handle_for_workdir(
            task_invocation.clone(),
            &task.worktree_path,
            Some(&task_cwd),
            Some(&repository_root),
        )
        .await?;
    let result = output.code_mode_result(&task_invocation.payload);
    if result.get("passed").and_then(Value::as_bool) == Some(true) {
        let head = tokio::task::spawn_blocking({
            let path = task.worktree_path.clone();
            move || {
                Command::new("git")
                    .current_dir(path)
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .map_err(|e| e.to_string())
            }
        })
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(FunctionCallError::RespondToModel)?;
        let (store, _, _) = task_store(&invocation).await?;
        let id = args.task_id;
        tokio::task::spawn_blocking(move || store.record_verification(&id, &head))
            .await
            .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
            .map_err(task_error)?;
    }
    Ok(JsonOutput(result))
}

async fn handle_integrate(invocation: ToolInvocation) -> Result<JsonOutput, FunctionCallError> {
    handle_integrate_with_cleanup(invocation, true).await
}

async fn handle_integrate_with_cleanup(
    invocation: ToolInvocation,
    cleanup: bool,
) -> Result<JsonOutput, FunctionCallError> {
    let args: TaskIdArgs = serde_json::from_str(&parse(
        invocation.payload.clone(),
        "flowdex task.integrate expects JSON arguments",
    )?)
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let _gate = task_gate(&args.task_id).await;
    let (store, _, _) = task_store(&invocation).await?;
    let result = tokio::task::spawn_blocking(move || {
        if cleanup {
            store.integrate(&args.task_id)
        } else {
            store.integrate_retained(&args.task_id)
        }
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(task_error)?;
    let commits = result.commits.into_iter().map(|commit| serde_json::json!({"sourceCommit": commit.source_commit, "integratedCommit": commit.integrated_commit.unwrap_or_default(), "agentId": commit.agent_id, "model": commit.model, "summary": truncate_message(&commit.summary)})).collect::<Vec<_>>();
    Ok(JsonOutput(
        serde_json::json!({"taskId": result.task_id, "commits": commits}),
    ))
}

/// Integrates a durable task directly from Rust scheduler code.
pub(crate) async fn integrate_task(
    mut invocation: ToolInvocation,
    task_id: String,
) -> Result<JsonOutput, FunctionCallError> {
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({"task_id": task_id}).to_string(),
    };
    handle_integrate(invocation).await
}

pub(crate) async fn integrate_task_retained(
    mut invocation: ToolInvocation,
    task_id: String,
) -> Result<JsonOutput, FunctionCallError> {
    invocation.payload = ToolPayload::Function {
        arguments: serde_json::json!({"task_id": task_id}).to_string(),
    };
    handle_integrate_with_cleanup(invocation, false).await
}

pub(crate) fn task_associated_agent(id: ThreadId) -> Option<String> {
    task_for_agent(id)
}
pub(crate) fn associate_task_agent(id: ThreadId, task: &str) {
    associate(id, task);
}

pub(crate) struct JsonOutput(pub(crate) Value);
impl ToolOutput for JsonOutput {
    fn log_preview(&self) -> String {
        self.0.to_string()
    }
    fn success_for_logging(&self) -> bool {
        true
    }
    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: codex_protocol::models::FunctionCallOutputPayload {
                body: codex_protocol::models::FunctionCallOutputBody::Text(self.0.to_string()),
                success: Some(true),
            },
        }
    }
    fn code_mode_result(&self, _payload: &ToolPayload) -> Value {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::repository_identity;
    use super::repository_root;
    use super::task_agent_instructions;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(cwd)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn repository_identity_is_shared_by_linked_worktrees() {
        let directory = tempdir().unwrap();
        let repository = directory.path().join("repository");
        let linked = directory.path().join("linked");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q"]);
        git(
            &repository,
            &["config", "user.email", "flowdex@example.com"],
        );
        git(&repository, &["config", "user.name", "Flowdex"]);
        fs::write(repository.join("README"), "base").unwrap();
        git(&repository, &["add", "README"]);
        git(&repository, &["commit", "-qm", "base"]);
        git(
            &repository,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );

        assert_eq!(
            repository_identity(&repository).unwrap(),
            repository_identity(&linked).unwrap()
        );
    }

    #[test]
    fn repository_root_resolves_from_nested_cwd() {
        let directory = tempdir().unwrap();
        let repository = directory.path().join("repository");
        let nested = repository.join("src").join("nested");
        fs::create_dir_all(&nested).unwrap();
        git(&repository, &["init", "-q"]);
        assert_eq!(
            repository_root(&nested).unwrap(),
            repository.canonicalize().unwrap()
        );
    }

    #[test]
    fn review_prompt_uses_supplied_review_instructions_only() {
        assert_eq!(
            task_agent_instructions(
                "stored task requirements",
                &["src/**".into()],
                &["src/lib.rs".into()],
                &["cargo test -p example".into()],
                "review the diff",
                true,
            ),
            "review the diff"
        );
    }

    #[test]
    fn worker_prompt_includes_stored_requirements_once() {
        let prompt = task_agent_instructions(
            "stored task requirements",
            &["src/**".into()],
            &["src/lib.rs".into()],
            &["cargo test -p example".into()],
            "implement the requirements above",
            false,
        );
        assert_eq!(prompt.matches("stored task requirements").count(), 1);
        assert!(prompt.contains("cargo test -p example"));
        assert!(prompt.contains("Flowdex will run these task verification commands automatically"));
    }
}
