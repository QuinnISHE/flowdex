use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_flowdex::AstGrepResult;
use codex_flowdex::ast_grep::run_ast_grep_rules_with_cancellation;
use codex_flowdex::discover_approved_rule_ids;
use codex_protocol::models::ResponseInputItem;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::path::Path;
use tokio_util::sync::CancellationToken;

const TOOL_NAME: &str = "flowdex_check_rules";
const SCAN_CANDIDATES_TOOL_NAME: &str = "scan_flowdex_rule_candidates";

pub(crate) struct FlowdexCheckRulesHandler;
pub(crate) struct FlowdexScanRuleCandidatesHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanRuleCandidatesArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckRulesArgs {
    rule_ids: Vec<String>,
}

impl ToolExecutor<ToolInvocation> for FlowdexCheckRulesHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: "Run approved Flowdex AST-grep rules.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(
                    "rule_ids".to_string(),
                    JsonSchema::array(JsonSchema::string(None), None),
                )]),
                Some(vec!["rule_ids".to_string()]),
                Some(false.into()),
            ),
            output_schema: Some(serde_json::json!({"type": "object"})),
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move { handle_check_rules(invocation).await.map(boxed_tool_output) })
    }
}

impl CoreToolRuntime for FlowdexCheckRulesHandler {}

impl ToolExecutor<ToolInvocation> for FlowdexScanRuleCandidatesHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SCAN_CANDIDATES_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: SCAN_CANDIDATES_TOOL_NAME.to_string(),
            description: "Scan resolved Flowdex findings for rule candidates.".to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(BTreeMap::new(), None, Some(false.into())),
            output_schema: Some(serde_json::json!({"type": "object"})),
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            handle_scan_rule_candidates(invocation)
                .await
                .map(boxed_tool_output)
        })
    }
}

impl CoreToolRuntime for FlowdexScanRuleCandidatesHandler {}

async fn handle_scan_rule_candidates(
    invocation: ToolInvocation,
) -> Result<RulesOutput, FunctionCallError> {
    if !matches!(invocation.source, ToolCallSource::Direct) {
        return Err(FunctionCallError::RespondToModel(
            "scan_flowdex_rule_candidates is available only to the direct model".into(),
        ));
    }
    let ToolPayload::Function { arguments } = &invocation.payload else {
        return Err(FunctionCallError::RespondToModel(
            "scan_flowdex_rule_candidates expects JSON arguments".into(),
        ));
    };
    let _: ScanRuleCandidatesArgs = parse_arguments(arguments)?;
    let (store, repository_root) =
        super::task::open_existing_store(&invocation.session, &invocation.turn).await?;
    let Some(store) = store else {
        return Ok(RulesOutput(serde_json::json!({"candidates": []})));
    };
    let threshold = u64::try_from(
        invocation
            .turn
            .config
            .flowdex_config
            .ast_grep_candidate_threshold,
    )
    .map_err(|_| FunctionCallError::RespondToModel("invalid Flowdex candidate threshold".into()))?;
    let result = tokio::task::spawn_blocking(move || {
        let rules_dir = repository_root.join(".flowdex/ast-grep/rules");
        let approved = if rules_dir.is_dir() {
            discover_approved_rule_ids(&repository_root).map_err(|_| scan_error())?
        } else {
            BTreeSet::new()
        };
        store
            .rule_candidates(threshold, &approved)
            .map_err(|_| scan_error())
    })
    .await
    .map_err(|_| scan_error())??;
    let value = serde_json::to_value(&result)
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
    Ok(RulesOutput(value))
}

fn scan_error() -> FunctionCallError {
    FunctionCallError::RespondToModel("Flowdex rule candidate scan failed".into())
}

pub(crate) async fn run_rules(
    trusted_repository_root: &Path,
    execution_root: &Path,
    rule_ids: Vec<String>,
    cancellation_token: &CancellationToken,
) -> Result<AstGrepResult, FunctionCallError> {
    if cancellation_token.is_cancelled() {
        return Err(FunctionCallError::RespondToModel(
            "rule verification cancelled".to_string(),
        ));
    }
    let trusted_repository_root = trusted_repository_root.to_path_buf();
    let execution_root = execution_root.to_path_buf();
    let scan_cancellation = cancellation_token.clone();
    let result = tokio::task::spawn_blocking(move || {
        run_ast_grep_rules_with_cancellation(
            &trusted_repository_root,
            &execution_root,
            &rule_ids,
            || scan_cancellation.is_cancelled(),
        )
    })
    .await
    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
    if cancellation_token.is_cancelled() {
        return Err(FunctionCallError::RespondToModel(
            "rule verification cancelled".to_string(),
        ));
    }
    Ok(result)
}

async fn handle_check_rules(invocation: ToolInvocation) -> Result<RulesOutput, FunctionCallError> {
    if !matches!(invocation.source, ToolCallSource::CodeMode { .. }) {
        return Err(FunctionCallError::RespondToModel(
            "flowdex.checkRules is available only inside a workflow".to_string(),
        ));
    }
    if !invocation.turn.config.active_project.is_trusted() {
        return Err(FunctionCallError::RespondToModel(
            "Flowdex rules require a trusted Git repository".to_string(),
        ));
    }
    let ToolPayload::Function { arguments } = &invocation.payload else {
        return Err(FunctionCallError::RespondToModel(
            "flowdex checkRules expects JSON arguments".to_string(),
        ));
    };
    let args: CheckRulesArgs = parse_arguments(arguments)?;
    validate_rule_ids(&args.rule_ids)?;

    let execution_root = invocation
        .turn
        .environments
        .single_local_environment_cwd()
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "flowdex.checkRules requires one local environment".to_string(),
            )
        })?;
    let result = run_rules(
        invocation.turn.config.cwd.as_path(),
        execution_root.as_path(),
        args.rule_ids,
        &invocation.cancellation_token,
    )
    .await?;
    Ok(RulesOutput(serde_json::to_value(result).map_err(
        |err| FunctionCallError::RespondToModel(err.to_string()),
    )?))
}

pub(crate) fn validate_rule_ids(rule_ids: &[String]) -> Result<(), FunctionCallError> {
    if rule_ids.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "rule_ids must be a non-empty array".to_string(),
        ));
    }
    if rule_ids.iter().any(|rule_id| rule_id.trim().is_empty()) {
        return Err(FunctionCallError::RespondToModel(
            "rule_ids must contain only non-empty strings".to_string(),
        ));
    }
    let unique = rule_ids.iter().collect::<HashSet<_>>();
    if unique.len() != rule_ids.len() {
        return Err(FunctionCallError::RespondToModel(
            "rule_ids must contain unique rule IDs".to_string(),
        ));
    }
    Ok(())
}

pub(crate) struct RulesOutput(Value);

impl ToolOutput for RulesOutput {
    fn log_output(&self) -> String {
        self.0.to_string()
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
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
