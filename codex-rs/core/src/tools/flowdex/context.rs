use super::task;
use crate::function_tool::FunctionCallError;
use crate::tools::context::{FunctionToolOutput, ToolInvocation, ToolPayload, boxed_tool_output};
use crate::tools::registry::{CoreToolRuntime, ToolExecutor};
use codex_flowdex::context::{ContextPackStatus, ContextPublication, ContextPublisher};
use codex_tools::{JsonSchema, ResponsesApiTool, ToolName, ToolSpec};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

const PUBLISH: &str = "publish_flowdex_context";
const READ: &str = "read_flowdex_context";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArgs {
    pack: String,
    key: String,
    path: String,
    line_start: u32,
    line_end: u32,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    pack: String,
}

pub(crate) struct PublishFlowdexContextHandler;
pub(crate) struct ReadFlowdexContextHandler;

impl ToolExecutor<ToolInvocation> for PublishFlowdexContextHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(PUBLISH)
    }
    fn spec(&self) -> ToolSpec {
        publish_spec()
    }
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move { publish_call(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for PublishFlowdexContextHandler {}

impl ToolExecutor<ToolInvocation> for ReadFlowdexContextHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(READ)
    }
    fn spec(&self) -> ToolSpec {
        read_spec()
    }
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move { read_call(invocation).await.map(boxed_tool_output) })
    }
}
impl CoreToolRuntime for ReadFlowdexContextHandler {}

fn publish_spec() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: PUBLISH.into(),
        description: "Publish one bounded source-backed fragment for the active Flowdex context collection. Use a stable descriptive key, a repository-relative path, and the smallest complete inclusive line range that proves one fact. Publish separate facts separately; publishing the same pack/key supersedes its prior version. Repository-lived packs also update their checked-in pack file in the task worktree, which Flowdex commits with successful task changes. The collection is incomplete until at least one fresh fragment is accepted. Do not paste source contents into a prose response.".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([
                ("pack".into(), JsonSchema::string(None)),
                ("key".into(), JsonSchema::string(None)),
                ("path".into(), JsonSchema::string(None)),
                ("line_start".into(), JsonSchema::integer(None)),
                ("line_end".into(), JsonSchema::integer(None)),
                ("summary".into(), JsonSchema::string(None)),
            ]),
            Some(vec![
                "pack".into(),
                "key".into(),
                "path".into(),
                "line_start".into(),
                "line_end".into(),
            ]),
            Some(false.into()),
        ),
        output_schema: Some(json!({"type":"object"})),
    })
}

fn read_spec() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: READ.into(),
        description: "Inspect the active Flowdex context pack associated with this task agent. Returns fresh, missing, or stale status and bounded source fragments. Normal dependent-task injection is automatic; use this only to diagnose or verify collection state.".into(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([(String::from("pack"), JsonSchema::string(None))]),
            Some(vec!["pack".into()]),
            Some(false.into()),
        ),
        output_schema: Some(json!({"type":"object"})),
    })
}

fn arguments(invocation: ToolInvocation, expected: &str) -> Result<String, FunctionCallError> {
    match invocation.payload {
        ToolPayload::Function { arguments } => Ok(arguments),
        _ => Err(FunctionCallError::RespondToModel(format!(
            "{expected} expects JSON arguments"
        ))),
    }
}

async fn associated_context(
    invocation: &ToolInvocation,
) -> Result<codex_flowdex::TaskRecord, FunctionCallError> {
    let task_id = task::associated_task(invocation.session.thread_id.clone()).ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "Flowdex context tools require an associated task agent".into(),
        )
    })?;
    let (store, _cwd, _identity) = task::task_store(invocation).await?;
    let task = tokio::task::spawn_blocking({
        let id = task_id;
        move || store.task(&id)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    Ok(task)
}

async fn publish_call(invocation: ToolInvocation) -> Result<FunctionToolOutput, FunctionCallError> {
    let args: PublishArgs = serde_json::from_str(&arguments(invocation.clone(), PUBLISH)?)
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let task_record = associated_context(&invocation).await?;
    let (store_for_info, _cwd, _identity) = task::task_store(&invocation).await?;
    let run_id = task_record.run_id.clone();
    let run_info = tokio::task::spawn_blocking(move || store_for_info.run_info(&run_id))
        .await
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let (store, _cwd, _identity) = task::task_store(&invocation).await?;
    let publication = ContextPublication {
        pack: args.pack,
        key: args.key,
        path: PathBuf::from(args.path),
        line_start: args.line_start,
        line_end: args.line_end,
        summary: args.summary,
    };
    let publisher = ContextPublisher {
        thread_id: Some(invocation.session.thread_id.to_string()),
        agent_id: invocation
            .turn
            .session_source
            .get_agent_path()
            .map(|p| p.to_string()),
    };
    let fragment = tokio::task::spawn_blocking(move || {
        store.publish_context_fragment(
            &task_record.run_id,
            &task_record.worktree_path,
            &run_info.integration_worktree,
            &publisher,
            &publication,
        )
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    Ok(FunctionToolOutput::from_text(
        json!({"pack": fragment.pack, "key": fragment.key, "version": fragment.version})
            .to_string(),
        Some(true),
    ))
}

async fn read_call(invocation: ToolInvocation) -> Result<FunctionToolOutput, FunctionCallError> {
    let args: ReadArgs = serde_json::from_str(&arguments(invocation.clone(), READ)?)
        .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let task_record = associated_context(&invocation).await?;
    let (store, _cwd, _identity) = task::task_store(&invocation).await?;
    let worktree = task_record.worktree_path.clone();
    let resolved = tokio::task::spawn_blocking(move || {
        store.resolve_context_pack(&task_record.run_id, &args.pack, &worktree)
    })
    .await
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?
    .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
    let status = match resolved.status {
        ContextPackStatus::Fresh => "fresh",
        ContextPackStatus::Missing => "missing",
        ContextPackStatus::Stale => "stale",
    };
    let fragments = resolved
        .fragments
        .into_iter()
        .map(|f| {
            json!({
                "key": f.key, "version": f.version, "path": f.path, "lineStart": f.line_start,
                "lineEnd": f.line_end, "summary": f.summary, "content": f.content,
            })
        })
        .collect::<Vec<_>>();
    Ok(FunctionToolOutput::from_text(
        json!({"pack": resolved.pack, "status": status, "fragments": fragments}).to_string(),
        Some(true),
    ))
}
