use crate::function_tool::FunctionCallError;
use crate::tools::context::{FunctionToolOutput, ToolInvocation, ToolPayload, boxed_tool_output};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{CoreToolRuntime, ToolExecutor};
use codex_flowdex::store::{FlowdexStore, ReviewFinding, ReviewOperation};
use codex_protocol::AgentPath;
use codex_tools::{JsonSchema, ResponsesApiTool, ToolName, ToolSpec};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

const REPORT: &str = "report_flowdex_review";
const MAX_FINDINGS: usize = 256;
const MAX_FILE_CHARS: usize = 4096;
const MAX_REASON_CHARS: usize = 16 * 1024;
const MAX_RULE_KEY_CHARS: usize = 512;

#[derive(Clone)]
pub(crate) struct ActiveReview {
    pub(crate) operation: ReviewOperation,
    pub(crate) store: Arc<FlowdexStore>,
    accepted: Arc<Mutex<bool>>,
}

static ACTIVE: OnceLock<Mutex<HashMap<AgentPath, ActiveReview>>> = OnceLock::new();

fn active() -> &'static Mutex<HashMap<AgentPath, ActiveReview>> {
    ACTIVE.get_or_init(Default::default)
}

/// Makes one strict report operation available to the specified agent path.
pub(crate) fn activate_review_agent(
    agent_path: AgentPath,
    operation: ReviewOperation,
    store: Arc<FlowdexStore>,
) {
    let review = ActiveReview {
        operation,
        store,
        accepted: Arc::new(Mutex::new(false)),
    };
    active()
        .lock()
        .expect("Flowdex review registry poisoned")
        .insert(agent_path, review);
}

pub(crate) fn deactivate_review_agent(agent_path: &AgentPath) -> bool {
    active()
        .lock()
        .expect("Flowdex review registry poisoned")
        .remove(agent_path)
        .and_then(|review| review.accepted.lock().ok().map(|accepted| *accepted))
        .unwrap_or(false)
}

pub(crate) fn review_report_tool_visible(agent_path: Option<&AgentPath>) -> bool {
    agent_path.is_some_and(|path| {
        active()
            .lock()
            .map(|reviews| reviews.contains_key(path))
            .unwrap_or(false)
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportArgs {
    findings: Vec<FindingArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FindingArgs {
    file: String,
    line_start: i64,
    line_end: i64,
    reason: String,
    #[serde(default)]
    rule_key: Option<String>,
    ast_grep_suitable: bool,
}

pub(crate) struct FlowdexReviewReportHandler;

impl ToolExecutor<ToolInvocation> for FlowdexReviewReportHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(REPORT)
    }

    fn spec(&self) -> ToolSpec {
        let finding = JsonSchema::object(
            BTreeMap::from([
                ("file".to_string(), JsonSchema::string(None)),
                ("lineStart".to_string(), JsonSchema::integer(None)),
                ("lineEnd".to_string(), JsonSchema::integer(None)),
                ("reason".to_string(), JsonSchema::string(None)),
                (
                    "ruleKey".to_string(),
                    JsonSchema::any_of(
                        vec![JsonSchema::string(None), JsonSchema::null(None)],
                        None,
                    ),
                ),
                ("astGrepSuitable".to_string(), JsonSchema::boolean(None)),
            ]),
            Some(vec![
                "file".to_string(),
                "lineStart".to_string(),
                "lineEnd".to_string(),
                "reason".to_string(),
                "ruleKey".to_string(),
                "astGrepSuitable".to_string(),
            ]),
            Some(false.into()),
        );
        ToolSpec::Function(ResponsesApiTool {
            name: REPORT.to_string(),
            description: "Submit the active Flowdex review's single durable result. Use findings: [] to pass. For each defect, point to the smallest current inclusive line range and explain the broken behavior and required correction. Set astGrepSuitable only for a repeatable syntax-shaped defect and provide a stable ruleKey. Do not guess attribution, message the worker, or finish with a separate prose verdict; Flowdex routes accepted findings automatically.".to_string(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(String::from("findings"), JsonSchema::array(finding, None))]),
                Some(vec![String::from("findings")]),
                Some(false.into()),
            ),
            output_schema: Some(serde_json::json!({"type": "object"})),
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move { self.handle_call(invocation).await })
    }
}

impl CoreToolRuntime for FlowdexReviewReportHandler {}

impl FlowdexReviewReportHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            turn,
            source,
            payload: ToolPayload::Function { arguments },
            ..
        } = invocation
        else {
            return Err(FunctionCallError::RespondToModel(
                "report_flowdex_review expects JSON arguments".to_string(),
            ));
        };
        if !matches!(source, crate::tools::context::ToolCallSource::Direct) {
            return Err(FunctionCallError::RespondToModel(
                "report_flowdex_review is available only to the active review agent".to_string(),
            ));
        }
        let args: ReportArgs = parse_arguments(&arguments)?;
        if args.findings.len() > MAX_FINDINGS {
            return Err(FunctionCallError::RespondToModel(
                "review report contains too many findings".to_string(),
            ));
        }
        let agent_path = turn.session_source.get_agent_path().ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "report_flowdex_review is available only to the active review agent".into(),
            )
        })?;
        let review = active()
            .lock()
            .map_err(|_| {
                FunctionCallError::RespondToModel("Flowdex review registry unavailable".into())
            })?
            .get(&agent_path)
            .cloned()
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "report_flowdex_review is unavailable outside an active review operation"
                        .into(),
                )
            })?;
        if *review.accepted.lock().map_err(|_| {
            FunctionCallError::RespondToModel("Flowdex review registry unavailable".into())
        })? {
            return Err(FunctionCallError::RespondToModel(
                "review report already accepted".into(),
            ));
        }
        let operation = review.operation.clone();
        let findings = args
            .findings
            .into_iter()
            .enumerate()
            .map(|(finding_order, finding)| {
                let file = trim_bounded(finding.file, MAX_FILE_CHARS, "file")?;
                let reason = trim_bounded(finding.reason, MAX_REASON_CHARS, "reason")?;
                if finding.line_start <= 0 || finding.line_end < finding.line_start {
                    return Err(FunctionCallError::RespondToModel(
                        "review finding lines must be positive and inclusive".into(),
                    ));
                }
                let rule_key = finding
                    .rule_key
                    .map(|rule| trim_bounded(rule, MAX_RULE_KEY_CHARS, "ruleKey"))
                    .transpose()?;
                Ok(ReviewFinding {
                    finding_id: Uuid::new_v4().to_string(),
                    operation_id: operation.operation_id.clone(),
                    finding_order: finding_order as i64,
                    file,
                    line_start: finding.line_start,
                    line_end: finding.line_end,
                    reason,
                    rule_key,
                    ast_grep_suitable: finding.ast_grep_suitable,
                    attributed_task_id: None,
                    attributed_operation_id: None,
                    attributed_agent_id: None,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let finding_count = findings.len();
        let store = Arc::clone(&review.store);
        tokio::task::spawn_blocking(move || {
            let mut accepted_operation = operation;
            accepted_operation.state = "accepted".to_string();
            store
                .record_review_operation(&accepted_operation)
                .map_err(|error| error.to_string())?;
            store
                .record_review_findings(&findings)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?
        .map_err(FunctionCallError::RespondToModel)?;
        if let Ok(mut accepted) = review.accepted.lock() {
            *accepted = true;
        }
        deactivate_review_agent(&agent_path);
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            serde_json::json!({"accepted": true, "findings": finding_count}).to_string(),
            Some(true),
        )))
    }
}

fn trim_bounded(value: String, limit: usize, field: &str) -> Result<String, FunctionCallError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{field} must be non-empty"
        )));
    }
    if value.chars().count() > limit {
        return Err(FunctionCallError::RespondToModel(format!(
            "{field} exceeds the bounded report limit"
        )));
    }
    Ok(value)
}
