use anyhow::Result;
use codex_features::Feature;
use codex_flowdex::{FlowdexStore, ReviewFinding, ReviewResolution, RunInfo, TaskDeclaration};
use codex_protocol::ThreadId;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::items::TurnItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use wiremock::Mock;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body).is_ok_and(|body| body.to_string().contains(text))
}

fn input_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body)
        .ok()
        .and_then(|body| body.get("input").cloned())
        .is_some_and(|input| input.to_string().contains(text))
}

fn has_function_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body).is_ok_and(|body| {
        body.get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("function_call_output")
                        && item.get("call_id").and_then(Value::as_str) == Some(call_id)
                })
            })
    })
}

fn function_call_output_text(request: &wiremock::Request, call_id: &str) -> Option<String> {
    serde_json::from_slice::<Value>(&request.body)
        .ok()?
        .get("input")?
        .as_array()?
        .iter()
        .find(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some(call_id)
        })?
        .get("output")?
        .as_str()
        .map(str::to_string)
}

async fn scan_candidate_call(
    mut builder: Box<core_test_support::test_codex::TestCodexBuilder>,
    prompt: &str,
) -> Result<Value> {
    let server = start_mock_server().await;
    let test = builder.build(&server).await?;
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("scan-resp-1"),
            ev_function_call("scan-call-1", "scan_flowdex_rule_candidates", "{}"),
            ev_completed("scan-resp-1"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("scan-resp-2"),
            ev_completed("scan-resp-2"),
        ]),
    )
    .await;
    test.submit_turn(prompt).await?;
    let output = follow_up
        .function_call_output_text("scan-call-1")
        .expect("scan tool output should be sent back to the model");
    Ok(serde_json::from_str(&output).unwrap_or(Value::String(output)))
}

fn run_scan_test(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()?
        .block_on(future)
}

#[test]
fn scan_flowdex_rule_candidates_rejects_untrusted_repository() -> Result<()> {
    run_scan_test(async {
        let builder = test_codex().with_config(|config| {
            config.active_project.trust_level = Some(TrustLevel::Untrusted);
        });
        let output = scan_candidate_call(Box::new(builder), "scan candidates").await?;
        assert!(
            output
                .as_str()
                .is_some_and(|text| text.contains("trusted Git repository"))
        );
        Ok(())
    })
}

#[test]
fn scan_flowdex_rule_candidates_rejects_missing_git_repository() -> Result<()> {
    run_scan_test(async {
        let builder = test_codex().with_config(|config| {
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        });
        let output = scan_candidate_call(Box::new(builder), "scan candidates").await?;
        assert!(
            output
                .as_str()
                .is_some_and(|text| text.contains("not a Git repository"))
        );
        Ok(())
    })
}

#[test]
fn scan_flowdex_rule_candidates_returns_empty_result() -> Result<()> {
    run_scan_test(async {
        let builder = test_codex()
            .with_config(|config| {
                config.active_project.trust_level = Some(TrustLevel::Trusted);
                config.flowdex_config.ast_grep_candidate_threshold = 1;
            })
            .with_workspace_setup(|cwd, _fs| async move {
                let git = |args: &[&str]| -> Result<()> {
                    let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                    anyhow::ensure!(output.status.success(), "git command failed");
                    Ok(())
                };
                git(&["init", "-q"])?;
                git(&["config", "user.name", "Flowdex Test"])?;
                git(&["config", "user.email", "flowdex-test@example.com"])?;
                fs::write(cwd.join("README.md"), "fixture\n")?;
                git(&["add", "."])?;
                git(&["commit", "-qm", "fixture"])?;
                Ok::<(), anyhow::Error>(())
            });
        let output = scan_candidate_call(Box::new(builder), "scan candidates").await?;
        assert_eq!(output, serde_json::json!({"candidates": []}));
        Ok(())
    })
}

#[test]
fn scan_flowdex_rule_candidates_first_use_does_not_create_store() -> Result<()> {
    run_scan_test(async {
        let mut builder = test_codex()
            .with_config(|config| {
                config.active_project.trust_level = Some(TrustLevel::Trusted);
            })
            .with_workspace_setup(|cwd, _fs| async move {
                let git = |args: &[&str]| -> Result<()> {
                    let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                    anyhow::ensure!(output.status.success(), "git command failed");
                    Ok(())
                };
                git(&["init", "-q"])?;
                git(&["config", "user.name", "Flowdex Test"])?;
                git(&["config", "user.email", "flowdex-test@example.com"])?;
                fs::write(cwd.join("README.md"), "fixture\n")?;
                git(&["add", "."])?;
                git(&["commit", "-qm", "fixture"])?;
                Ok::<(), anyhow::Error>(())
            });
        let server = start_mock_server().await;
        let test = builder.build(&server).await?;
        mount_sse_once(
            &server,
            sse(vec![
                ev_response_created("scan-resp-1"),
                ev_function_call("scan-call-1", "scan_flowdex_rule_candidates", "{}"),
                ev_completed("scan-resp-1"),
            ]),
        )
        .await;
        let follow_up = mount_sse_once(
            &server,
            sse(vec![
                ev_response_created("scan-resp-2"),
                ev_completed("scan-resp-2"),
            ]),
        )
        .await;
        test.submit_turn("scan candidates").await?;
        assert_eq!(
            follow_up
                .function_call_output_text("scan-call-1")
                .as_deref(),
            Some(r#"{"candidates":[]}"#)
        );
        assert!(!test.codex_home_path().join("flowdex").exists());
        Ok(())
    })
}

#[test]
fn scan_flowdex_rule_candidates_serializes_candidate_result() -> Result<()> {
    run_scan_test(async {
        let mut builder = test_codex()
            .with_config(|config| {
                config.active_project.trust_level = Some(TrustLevel::Trusted);
                config.flowdex_config.ast_grep_candidate_threshold = 1;
            })
            .with_workspace_setup(|cwd, _fs| async move {
                let git = |args: &[&str]| -> Result<()> {
                    let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                    anyhow::ensure!(output.status.success(), "git command failed");
                    Ok(())
                };
                git(&["init", "-q"])?;
                git(&["config", "user.name", "Flowdex Test"])?;
                git(&["config", "user.email", "flowdex-test@example.com"])?;
                fs::write(cwd.join("README.md"), "fixture\n")?;
                git(&["add", "."])?;
                git(&["commit", "-qm", "fixture"])?;
                Ok::<(), anyhow::Error>(())
            });
        let server = start_mock_server().await;
        let test = builder.build(&server).await?;
        let workspace = test.workspace_path(".");
        let codex_home = test.codex_home_path().to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let identity = String::from_utf8(
                Command::new("git")
                    .current_dir(&workspace)
                    .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                    .output()?
                    .stdout,
            )?
            .trim()
            .to_string();
            let identity = fs::canonicalize(identity)?.to_string_lossy().into_owned();
            let store = FlowdexStore::open(&codex_home, identity.clone(), &workspace)?;
            let run = RunInfo {
                run_id: "scan-run".into(),
                parent_thread_id: "thread".into(),
                workflow_path: ".flowdex/workflows/test.js".into(),
                parent_run_id: None,
                workflow_identity: None,
                repository_identity: identity,
                integration_worktree: workspace.clone(),
            };
            let task = store.create_task(
                &run,
                &TaskDeclaration {
                    id: "scan-task".into(),
                    name: "scan task".into(),
                    instructions: "repair".into(),
                    read_scope: vec![],
                    write_scope: vec![],
                    verification: vec![],
                },
            )?;
            let operation = store.start_operation("scan-task", "scan-op", "agent", "model")?;
            fs::write(task.worktree_path.join("repair.rs"), "repair\n")?;
            Command::new("git")
                .current_dir(&task.worktree_path)
                .args(["add", "."])
                .status()?;
            Command::new("git")
                .current_dir(&task.worktree_path)
                .args(["commit", "-qm", "repair"])
                .status()?;
            let commits =
                store.finish_operation("scan-task", &operation.operation_id, "completed")?;
            let integrated = store.integrate("scan-task")?;
            let findings = (0..3)
                .map(|index| ReviewFinding {
                    finding_id: format!("scan-finding-{index}"),
                    operation_id: "review-op".into(),
                    finding_order: index,
                    file: format!("src/layout-{index}.rs"),
                    line_start: 42 + index,
                    line_end: 44 + index,
                    reason: "The cast bypasses the checked layout helper.".into(),
                    rule_key: Some("avoid-unchecked-layout-cast".into()),
                    ast_grep_suitable: true,
                    attributed_task_id: None,
                    attributed_operation_id: None,
                    attributed_agent_id: None,
                })
                .collect::<Vec<_>>();
            store.record_review_findings(&findings)?;
            for finding in findings {
                store.record_review_resolution(&ReviewResolution {
                    finding_id: finding.finding_id,
                    repair_operation_id: operation.operation_id.clone(),
                    source_commit: commits[0].source_commit.clone(),
                    integrated_commit: integrated.commits[0].integrated_commit.clone(),
                })?;
            }
            Ok(())
        })
        .await??;
        mount_sse_once(
            &server,
            sse(vec![
                ev_response_created("scan-resp-1"),
                ev_function_call("scan-call-1", "scan_flowdex_rule_candidates", "{}"),
                ev_completed("scan-resp-1"),
            ]),
        )
        .await;
        let follow_up = mount_sse_once(
            &server,
            sse(vec![
                ev_response_created("scan-resp-2"),
                ev_completed("scan-resp-2"),
            ]),
        )
        .await;
        test.submit_turn("scan candidates").await?;
        let output = follow_up
            .function_call_output_text("scan-call-1")
            .expect("scan output");
        let output: Value = serde_json::from_str(&output)?;
        assert_eq!(
            output["candidates"][0]["ruleKey"],
            "avoid-unchecked-layout-cast"
        );
        assert_eq!(output["candidates"][0]["resolvedOccurrences"], 3);
        assert_eq!(output["candidates"][0]["examples"][0]["lineStart"], 42);
        assert_eq!(
            output["candidates"][0]["examples"][0]["integratedCommit"]
                .as_str()
                .map(str::len),
            Some(40)
        );
        Ok(())
    })
}

#[derive(Clone, Default)]
struct SchedulerAgentResponder {
    alpha_requests: Arc<AtomicUsize>,
    beta_requests: Arc<AtomicUsize>,
}

#[derive(Clone, Default)]
struct ContextPackResponder {
    collector_requests: Arc<AtomicUsize>,
    collector_completed: Arc<AtomicBool>,
    independent_requests: Arc<AtomicUsize>,
    independent_started_before_collector: Arc<AtomicBool>,
    first_requests: Arc<AtomicUsize>,
    second_requests: Arc<AtomicUsize>,
}

impl Respond for ContextPackResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        if body_contains(request, "Collect context pack") {
            let publish_output = has_function_call_output(request, "publish-context");
            if !publish_output {
                self.collector_requests.fetch_add(1, Ordering::SeqCst);
            }
            if has_function_call_output(request, "publish-context") {
                self.collector_completed.store(true, Ordering::SeqCst);
                return sse_response(sse(vec![
                    ev_response_created("resp-context-collector-done"),
                    ev_assistant_message("msg-context-collector", "published"),
                    ev_completed("resp-context-collector-done"),
                ]));
            }
            return sse_response(sse(vec![
                ev_response_created("resp-context-collector-call"),
                ev_function_call(
                    "publish-context",
                    "publish_flowdex_context",
                    &serde_json::json!({
                        "pack": "fixture",
                        "key": "context",
                        "path": "context.txt",
                        "line_start": 1,
                        "line_end": 1,
                    })
                    .to_string(),
                ),
                ev_completed("resp-context-collector-call"),
            ]))
            .set_delay(Duration::from_secs(2));
        }
        if body_contains(request, "independent instructions") {
            self.independent_requests.fetch_add(1, Ordering::SeqCst);
            if !self.collector_completed.load(Ordering::SeqCst) {
                self.independent_started_before_collector
                    .store(true, Ordering::SeqCst);
            }
            return sse_response(sse(vec![
                ev_response_created("resp-context-independent"),
                ev_assistant_message("msg-context-independent", "independent complete"),
                ev_completed("resp-context-independent"),
            ]));
        }
        if body_contains(request, "first instructions") {
            self.first_requests.fetch_add(1, Ordering::SeqCst);
            return sse_response(sse(vec![
                ev_response_created("resp-context-first"),
                ev_assistant_message("msg-context-first", "first complete"),
                ev_completed("resp-context-first"),
            ]))
            .set_delay(Duration::from_secs(4));
        }
        if body_contains(request, "second instructions") {
            self.second_requests.fetch_add(1, Ordering::SeqCst);
            return sse_response(sse(vec![
                ev_response_created("resp-context-second"),
                ev_assistant_message("msg-context-second", "second complete"),
                ev_completed("resp-context-second"),
            ]));
        }
        if has_function_call_output(request, "call-context-wait") {
            return sse_response(sse(vec![
                ev_response_created("resp-context-parent-done"),
                ev_completed("resp-context-parent-done"),
            ]));
        }
        if has_function_call_output(request, "call-context-outer") {
            let output: Value = function_call_output_text(request, "call-context-outer")
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            if output["status"] == "yielded" {
                return sse_response(sse(vec![
                    ev_response_created("resp-context-parent-wait"),
                    ev_function_call(
                        "call-context-wait",
                        "wait_flowdex_workflow",
                        &serde_json::json!({ "run_id": output["runId"] }).to_string(),
                    ),
                    ev_completed("resp-context-parent-wait"),
                ]));
            }
            return sse_response(sse(vec![
                ev_response_created("resp-context-parent-done"),
                ev_completed("resp-context-parent-done"),
            ]));
        }
        if body_contains(request, "run the context workflow") {
            return sse_response(sse(vec![
                ev_response_created("resp-context-parent"),
                ev_function_call(
                    "call-context-outer",
                    "start_flowdex_workflow",
                    &serde_json::json!({"path": ".flowdex/workflows/context.js"}).to_string(),
                ),
                ev_completed("resp-context-parent"),
            ]));
        }
        sse_response(sse(vec![
            ev_response_created("resp-context-empty"),
            ev_completed("resp-context-empty"),
        ]))
    }
}

#[derive(Clone, Default)]
struct NestedWorkflowResponder {
    parent_requests: Arc<AtomicUsize>,
}

impl Respond for NestedWorkflowResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        if body_contains(request, "run nested workflow")
            && self.parent_requests.fetch_add(1, Ordering::SeqCst) == 0
        {
            return sse_response(sse(vec![
                ev_response_created("resp-nested-parent-1"),
                ev_function_call(
                    "call-nested-parent",
                    "start_flowdex_workflow",
                    &serde_json::json!({
                        "path": ".flowdex/workflows/parent.js",
                        "input": {"runChild": true, "files": ["child.txt"]},
                    })
                    .to_string(),
                ),
                ev_completed("resp-nested-parent-1"),
            ]));
        }
        if has_function_call_output(request, "call-nested-parent") {
            let output: Value = function_call_output_text(request, "call-nested-parent")
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            if output["status"] == "yielded" {
                return sse_response(sse(vec![
                    ev_response_created("resp-nested-parent-wait"),
                    ev_function_call(
                        "call-nested-wait",
                        "wait_flowdex_workflow",
                        &serde_json::json!({ "run_id": output["runId"] }).to_string(),
                    ),
                    ev_completed("resp-nested-parent-wait"),
                ]));
            }
            return sse_response(sse(vec![
                ev_response_created("resp-nested-parent-2"),
                ev_completed("resp-nested-parent-2"),
            ]));
        }
        if has_function_call_output(request, "call-nested-wait") {
            return sse_response(sse(vec![
                ev_response_created("resp-nested-parent-2"),
                ev_completed("resp-nested-parent-2"),
            ]));
        }
        sse_response(sse(vec![
            ev_response_created("resp-nested-empty"),
            ev_completed("resp-nested-empty"),
        ]))
    }
}

impl Respond for SchedulerAgentResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        if has_function_call_output(request, "call-scheduler-wait") {
            return sse_response(sse(vec![
                ev_response_created("resp-scheduler-parent-2"),
                ev_completed("resp-scheduler-parent-2"),
            ]));
        }
        if has_function_call_output(request, "call-scheduler-outer") {
            let output: Value = function_call_output_text(request, "call-scheduler-outer")
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            if output["status"] == "yielded" {
                return sse_response(sse(vec![
                    ev_response_created("resp-scheduler-parent-wait"),
                    ev_function_call(
                        "call-scheduler-wait",
                        "wait_flowdex_workflow",
                        &serde_json::json!({ "run_id": output["runId"] }).to_string(),
                    ),
                    ev_completed("resp-scheduler-parent-wait"),
                ]));
            }
            return sse_response(sse(vec![
                ev_response_created("resp-scheduler-parent-2"),
                ev_completed("resp-scheduler-parent-2"),
            ]));
        }
        if body_contains(request, "alpha instructions") {
            self.alpha_requests.fetch_add(1, Ordering::SeqCst);
            return sse_response(sse(vec![
                ev_response_created("resp-scheduler-alpha-1"),
                ev_assistant_message("msg-scheduler-alpha", "alpha complete with no changes"),
                ev_completed("resp-scheduler-alpha-1"),
            ]))
            .set_delay(Duration::from_secs(3));
        }
        if body_contains(request, "beta instructions") {
            self.beta_requests.fetch_add(1, Ordering::SeqCst);
            return sse_response(sse(vec![
                ev_response_created("resp-scheduler-beta-1"),
                ev_assistant_message("msg-scheduler-beta", "beta complete with no changes"),
                ev_completed("resp-scheduler-beta-1"),
            ]))
            .set_delay(Duration::from_secs(3));
        }
        if body_contains(request, "join instructions") {
            return sse_response(sse(vec![
                ev_response_created("resp-scheduler-join-1"),
                ev_assistant_message("msg-scheduler-join", "join complete with no changes"),
                ev_completed("resp-scheduler-join-1"),
            ]));
        }
        if body_contains(request, "run the scheduler workflow") {
            return sse_response(sse(vec![
                ev_response_created("resp-scheduler-parent-1"),
                ev_function_call(
                    "call-scheduler-outer",
                    "start_flowdex_workflow",
                    &serde_json::json!({
                        "path": ".flowdex/workflows/scheduler.js"
                    })
                    .to_string(),
                ),
                ev_completed("resp-scheduler-parent-1"),
            ]));
        }
        sse_response(sse(vec![
            ev_response_created("resp-scheduler-empty"),
            ev_completed("resp-scheduler-empty"),
        ]))
    }
}

#[derive(Clone, Default)]
struct JoinedFlowResponder {
    alpha_requests: Arc<AtomicUsize>,
    beta_requests: Arc<AtomicUsize>,
    collector_requests: Arc<AtomicUsize>,
    reviewer_requests: Arc<AtomicUsize>,
    parent_steps: Arc<AtomicUsize>,
}

impl Respond for JoinedFlowResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let control_output = serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|body| body.get("input").cloned())
            .and_then(|input| input.as_array().cloned())
            .and_then(|items| {
                items.into_iter().rev().find_map(|item| {
                    let call_id = item.get("call_id").and_then(Value::as_str)?;
                    if item.get("type").and_then(Value::as_str) != Some("function_call_output")
                        || (call_id != "call-joined-start"
                            && !call_id.starts_with("call-joined-control-"))
                    {
                        return None;
                    }
                    item.get("output")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
            })
            .and_then(|output| serde_json::from_str::<Value>(&output).ok());
        if let Some(output) = control_output {
            let action = match output["status"].as_str() {
                Some("yielded") => Some("wait_flowdex_workflow"),
                Some("boundary") => Some("continue_flowdex_workflow"),
                _ => None,
            };
            if let Some(action) = action {
                let step = self.parent_steps.fetch_add(1, Ordering::SeqCst);
                let response_id = format!("resp-joined-control-{step}");
                let call_id = format!("call-joined-control-{step}");
                return sse_response(sse(vec![
                    ev_response_created(&response_id),
                    ev_function_call(
                        &call_id,
                        action,
                        &serde_json::json!({"run_id": output["runId"]}).to_string(),
                    ),
                    ev_completed(&response_id),
                ]));
            }
            return sse_response(sse(vec![
                ev_response_created("resp-joined-done"),
                ev_completed("resp-joined-done"),
            ]));
        }
        if body_contains(request, "Collect context pack") {
            if has_function_call_output(request, "call-joined-publish") {
                return sse_response(sse(vec![
                    ev_response_created("resp-joined-collector-done"),
                    ev_assistant_message("msg-joined-collector", "context published"),
                    ev_completed("resp-joined-collector-done"),
                ]));
            }
            self.collector_requests.fetch_add(1, Ordering::SeqCst);
            return sse_response(sse(vec![
                ev_response_created("resp-joined-collector"),
                ev_function_call(
                    "call-joined-publish",
                    "publish_flowdex_context",
                    &serde_json::json!({
                        "pack": "joined",
                        "key": "context",
                        "path": "context.txt",
                        "line_start": 1,
                        "line_end": 1,
                    })
                    .to_string(),
                ),
                ev_completed("resp-joined-collector"),
            ]));
        }
        if body_contains(
            request,
            "Submit exactly one report with report_flowdex_review",
        ) {
            if has_function_call_output(request, "call-joined-review") {
                return sse_response(sse(vec![
                    ev_response_created("resp-joined-review-done"),
                    ev_assistant_message("msg-joined-review", "review accepted"),
                    ev_completed("resp-joined-review-done"),
                ]));
            }
            self.reviewer_requests.fetch_add(1, Ordering::SeqCst);
            return sse_response(sse(vec![
                ev_response_created("resp-joined-review"),
                ev_function_call(
                    "call-joined-review",
                    "report_flowdex_review",
                    r#"{"findings":[]}"#,
                ),
                ev_completed("resp-joined-review"),
            ]));
        }
        let task = if body_contains(request, "alpha joined instructions") {
            self.alpha_requests.fetch_add(1, Ordering::SeqCst);
            Some(("resp-joined-alpha", "msg-joined-alpha", true))
        } else if body_contains(request, "beta joined instructions") {
            self.beta_requests.fetch_add(1, Ordering::SeqCst);
            Some(("resp-joined-beta", "msg-joined-beta", true))
        } else if body_contains(request, "dependent joined instructions") {
            Some(("resp-joined-dependent", "msg-joined-dependent", false))
        } else if body_contains(request, "later joined instructions") {
            Some(("resp-joined-later", "msg-joined-later", false))
        } else {
            None
        };
        if let Some((response_id, message_id, delayed)) = task {
            let response = sse_response(sse(vec![
                ev_response_created(response_id),
                ev_assistant_message(message_id, "task complete with no changes"),
                ev_completed(response_id),
            ]));
            if delayed {
                return response.set_delay(Duration::from_secs(2));
            }
            return response;
        }
        if body_contains(request, "run the joined workflow") {
            return sse_response(sse(vec![
                ev_response_created("resp-joined-parent"),
                ev_function_call(
                    "call-joined-start",
                    "start_flowdex_workflow",
                    &serde_json::json!({
                        "path": ".flowdex/workflows/joined.js",
                        "input": {"project": "joined"},
                    })
                    .to_string(),
                ),
                ev_completed("resp-joined-parent"),
            ]));
        }
        sse_response(sse(vec![
            ev_response_created("resp-joined-empty"),
            ev_completed("resp-joined-empty"),
        ]))
    }
}

fn flowdex_contains_file(root: &Path, name: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.file_name().and_then(|file| file.to_str()) == Some(name)
            || (path.is_dir() && flowdex_contains_file(&path, name))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_flowdex_workflow_executes_saved_v8_module() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.features.disable(Feature::Collab).unwrap();
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("hello.js"),
                "await Promise.resolve(); text(JSON.stringify({ input: flowdex.input, path: flowdex.workflowPath, spawnAvailable: typeof tools.flowdex_spawn_agent === 'function' }));",
            )?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({
        "path": ".flowdex/workflows/hello.js",
        "input": { "answer": 42 },
    });
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call("call-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    test.submit_turn("start the saved workflow").await?;

    let output = follow_up
        .function_call_output_text("call-1")
        .expect("start tool output should be sent back to the model");
    let output: Value = serde_json::from_str(&output)?;
    assert!(output["runId"].as_str().is_some());
    assert_eq!(output["status"], "completed");
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["input"]["answer"], 42);
    assert_eq!(workflow_output["path"], ".flowdex/workflows/hello.js");
    assert_eq!(workflow_output["spawnAvailable"], false);
    assert!(output.get("error").is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_flowdex_workflow_bounds_javascript_errors() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("error.js"),
                "throw new Error('x'.repeat(100000));",
            )?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/error.js" });
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-error-1"),
            ev_function_call("call-error-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-error-1"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-error-2"),
            ev_completed("resp-error-2"),
        ]),
    )
    .await;

    test.submit_turn("start the failing workflow").await?;

    let output = follow_up
        .function_call_output_text("call-error-1")
        .expect("start tool output should be sent back to the model");
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "failed");
    let error = output["error"].as_str().expect("error should be present");
    assert!(error.len() < 100_000);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flowdex_workflow_spawns_and_waits_without_parent_completion_notification() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("agents.js"),
                "const agentId = await flowdex.spawnAgent({ name: 'worker', instructions: 'child instructions', model: 'gpt-5.4' });\nconst result = await flowdex.waitAgent(agentId);\ntext(JSON.stringify({ hidden: typeof tools.flowdex_spawn_agent === 'function', recursive: typeof tools.start_flowdex_workflow === 'undefined', result }));",
            )?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/agents.js" });

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "start the agent workflow"),
        sse(vec![
            ev_response_created("resp-agents-1"),
            ev_function_call("call-agents-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-agents-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-task-child-1"),
        sse(vec![
            ev_response_created("resp-task-child-1-followup"),
            ev_assistant_message("msg-task-child-1", "initial task change committed"),
            ev_completed("resp-task-child-1-followup"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "child instructions"),
        sse(vec![
            ev_response_created("resp-child-1"),
            ev_assistant_message("msg-child-1", "child output"),
            ev_completed("resp-child-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-task-child-2"),
        sse(vec![
            ev_response_created("resp-task-child-2-followup"),
            ev_assistant_message("msg-task-child-2", "resumed task change committed"),
            ev_completed("resp-task-child-2-followup"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-agents-1"),
        sse(vec![
            ev_response_created("resp-agents-2"),
            ev_completed("resp-agents-2"),
        ]),
    )
    .await;

    test.submit_turn("start the agent workflow").await?;

    let output = follow_up
        .function_call_output_text("call-agents-1")
        .expect("start tool output should be sent back to the model");
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "completed");
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["hidden"], true);
    assert_eq!(workflow_output["recursive"], true);
    assert_eq!(workflow_output["result"]["status"], "completed");
    assert_eq!(workflow_output["result"]["message"], "child output");
    let follow_up_request = follow_up.single_request();
    assert!(!follow_up_request.body_contains_text("Message Type: FINAL_ANSWER"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flowdex_resume_agent_context_modes_are_submission_owned() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("resume.js"),
                "const agentId = await flowdex.spawnAgent({ name: 'worker', instructions: 'initial instructions', model: 'gpt-5.4', reasoningEffort: 'high' });\nconst initial = await flowdex.waitAgent(agentId);\nconst keep = await flowdex.resumeAgent(agentId, 'keep instructions');\nconst compact = await flowdex.resumeAgent(agentId, 'compact instructions', { contextMode: 'compact' });\nconst handoff = await flowdex.resumeAgent(agentId, 'new instructions', { contextMode: 'handoff' });\nconst failureId = await flowdex.spawnAgent({ name: 'failure_worker', instructions: 'failure initial', model: 'gpt-5.4' });\nawait flowdex.waitAgent(failureId);\nconst failedCompact = await flowdex.resumeAgent(failureId, 'must not dispatch', { contextMode: 'compact' });\nlet primitiveOptionsRejected = false;\ntry { await flowdex.resumeAgent(agentId, 'invalid primitive options', 'compact'); } catch { primitiveOptionsRejected = true; }\nlet unknownOptionsRejected = false;\ntry { await flowdex.resumeAgent(agentId, 'invalid unknown options', { timeoutMs: 1 }); } catch { unknownOptionsRejected = true; }\ntext(JSON.stringify({ initial, keep, compact, handoff, failedCompact, primitiveOptionsRejected, unknownOptionsRejected }));",
            )?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/resume.js" });

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "start the resume workflow"),
        sse(vec![
            ev_response_created("resp-resume-parent-1"),
            ev_function_call("call-resume-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-resume-parent-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "initial instructions"),
        sse(vec![
            ev_response_created("resp-resume-child-1"),
            ev_assistant_message("msg-resume-child-1", "initial output"),
            ev_completed("resp-resume-child-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "keep instructions"),
        sse(vec![
            ev_response_created("resp-resume-child-2"),
            ev_assistant_message("msg-resume-child-2", "keep output"),
            ev_completed("resp-resume-child-2"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "initial instructions") && body_contains(request, "compaction")
        },
        sse(vec![
            ev_response_created("resp-resume-compact"),
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "COMPACTED_CONTEXT",
                }
            }),
            ev_completed("resp-resume-compact"),
        ]),
    )
    .await;
    let replacement = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "compact instructions"),
        sse(vec![
            ev_response_created("resp-resume-child-3"),
            ev_assistant_message("msg-resume-child-3", "compact output"),
            ev_completed("resp-resume-child-3"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "failure initial"),
        sse(vec![
            ev_response_created("resp-resume-failure-initial"),
            ev_assistant_message("msg-resume-failure-initial", "failure ready"),
            ev_completed("resp-resume-failure-initial"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "failure initial") && body_contains(request, "compaction")
        },
        sse(vec![
            ev_response_created("resp-resume-failed-compact"),
            ev_assistant_message("msg-resume-failed-compact", "invalid compact output"),
            ev_completed("resp-resume-failed-compact"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "Produce only a concise structured handoff")
        },
        sse(vec![
            ev_response_created("resp-resume-handoff"),
            ev_assistant_message(
                "msg-resume-handoff",
                "completed work: initial; remaining work: new instructions",
            ),
            ev_completed("resp-resume-handoff"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "new instructions"),
        sse(vec![
            ev_response_created("resp-resume-replacement"),
            ev_assistant_message("msg-resume-replacement", "replacement output"),
            ev_completed("resp-resume-replacement"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-resume-1"),
        sse(vec![
            ev_response_created("resp-resume-parent-2"),
            ev_completed("resp-resume-parent-2"),
        ]),
    )
    .await;

    test.submit_turn("start the resume workflow").await?;

    let output = follow_up
        .function_call_output_text("call-resume-1")
        .expect("start tool output should be sent back to the model");
    let output: Value = serde_json::from_str(&output)?;
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["initial"]["status"], "completed");
    assert_eq!(workflow_output["keep"]["message"], "keep output");
    assert_eq!(workflow_output["compact"]["message"], "compact output");
    assert_eq!(workflow_output["handoff"]["message"], "replacement output");
    assert_ne!(
        workflow_output["handoff"]["agentId"],
        workflow_output["initial"]["agentId"]
    );
    assert_eq!(workflow_output["failedCompact"]["status"], "errored");
    assert_eq!(workflow_output["primitiveOptionsRejected"], true);
    assert_eq!(workflow_output["unknownOptionsRejected"], true);
    let initial_id =
        ThreadId::from_string(workflow_output["initial"]["agentId"].as_str().unwrap())?;
    let replacement_id =
        ThreadId::from_string(workflow_output["handoff"]["agentId"].as_str().unwrap())?;
    let initial_snapshot = test
        .thread_manager
        .get_thread(initial_id)
        .await?
        .config_snapshot()
        .await;
    let replacement_snapshot = test
        .thread_manager
        .get_thread(replacement_id)
        .await?
        .config_snapshot()
        .await;
    assert_eq!(
        replacement_snapshot.parent_thread_id,
        initial_snapshot.parent_thread_id
    );
    assert_eq!(replacement_snapshot.model, "gpt-5.4");
    assert_eq!(
        replacement_snapshot.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    let initial_path = initial_snapshot.session_source.get_agent_path().unwrap();
    let replacement_path = replacement_snapshot
        .session_source
        .get_agent_path()
        .unwrap();
    assert_eq!(initial_path.as_str(), "/root/worker");
    assert!(replacement_path.as_str().starts_with("/root/handoff_"));
    assert!(!replacement_path.as_str().contains("/root/worker/handoff_"));
    let depth = |source: &SessionSource| match source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => Some(*depth),
        _ => None,
    };
    assert_eq!(
        depth(&replacement_snapshot.session_source),
        depth(&initial_snapshot.session_source)
    );
    let replacement_request = replacement.single_request();
    assert_eq!(replacement_request.body_json()["model"], "gpt-5.4");
    let follow_up_request = follow_up.single_request();
    assert!(!follow_up_request.body_contains_text("completed work: initial"));
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .all(|request| !body_contains(request, "must not dispatch"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flowdex_workflow_verifies_commands_in_order() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::CodeMode).unwrap();
    });
    builder = builder.with_workspace_setup(|cwd, _fs| async move {
        let workflow_dir = cwd.join(".flowdex/workflows");
        fs::create_dir_all(&workflow_dir)?;
        fs::write(
            workflow_dir.join("verify.js"),
            "const result = await flowdex.verify(['echo first', 'exit 7', 'echo after']); text(JSON.stringify(result));",
        )?;
        Ok::<(), anyhow::Error>(())
    });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/verify.js" });
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-verify-1"),
            ev_function_call("call-verify-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-verify-1"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-verify-2"),
            ev_completed("resp-verify-2"),
        ]),
    )
    .await;

    test.submit_turn("run verification").await?;

    let output = follow_up
        .function_call_output_text("call-verify-1")
        .expect("verification output should be returned");
    let output: Value = serde_json::from_str(&output)?;
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["passed"], false);
    let commands = workflow_output["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["command"], "echo first");
    assert_eq!(commands[0]["exitCode"], 0);
    assert!(commands[0].get("output").is_none());
    assert_eq!(commands[1]["command"], "exit 7");
    assert_eq!(commands[1]["exitCode"], 7);
    assert!(commands[1]["durationMs"].is_u64());
    if let Some(output) = commands[1].get("output") {
        assert!(output.as_str().is_some());
    }
    Ok(())
}

#[test]
fn flowdex_rules_run_during_verification_and_explicitly() -> Result<()> {
    // V8-backed workflows exceed Windows' default Tokio worker stack.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()?
        .block_on(async {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        })
        .with_pre_build_hook(|home| {
            fs::write(home.join("flowdex.toml"), "ast_grep_always_run = [\"no-console\"]\n")
                .expect("global Flowdex config");
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let rules_dir = cwd.join(".flowdex/ast-grep/rules");
            fs::create_dir_all(&rules_dir)?;
            fs::write(
                rules_dir.join("no-console.yml"),
                "id: no-console\nlanguage: JavaScript\nrule:\n  pattern: console.log($$$)\nmessage: avoid console.log\nseverity: warning\n",
            )?;
            fs::write(cwd.join("fixture.js"), "console.log('fixture');\n")?;
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("rules.js"),
                "const verification = await flowdex.verify(['echo verified']); const explicit = await flowdex.checkRules(['no-console']); text(JSON.stringify({ verification, explicit }));",
            )?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/rules.js" });
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-rules-1"),
            ev_function_call("call-rules-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-rules-1"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-rules-2"),
            ev_completed("resp-rules-2"),
        ]),
    )
    .await;

    test.submit_turn("run the configured rules").await?;

    let output = follow_up
        .function_call_output_text("call-rules-1")
        .expect("workflow output should be returned");
    let output: Value = serde_json::from_str(&output)?;
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["verification"]["passed"], false);
    assert_eq!(workflow_output["verification"]["rules"]["passed"], false);
    assert_eq!(
        workflow_output["verification"]["rules"]["findings"][0]["ruleId"],
        "no-console"
    );
    assert_eq!(workflow_output["explicit"]["passed"], false);
    assert_eq!(
        workflow_output["explicit"]["findings"][0]["file"],
        "fixture.js"
    );
            Ok(())
        })
}

#[test]
fn flowdex_task_lifecycle_attributes_commits_and_cleans_up() -> Result<()> {
    // V8-backed task agents exceed Windows' default Tokio worker stack.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()?
        .block_on(async {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let run_git = |args: &[&str]| -> Result<()> {
                let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                if !output.status.success() {
                    anyhow::bail!(
                        "git {args:?} failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Ok(())
            };
            fs::create_dir_all(cwd.join(".flowdex/workflows"))?;
            fs::write(
                cwd.join(".flowdex/workflows/task.js"),
                r#"let createUnknownRejected = false;
try {
  await flowdex.createTask({ name: 'invalid', instructions: 'invalid', extra: true });
} catch { createUnknownRejected = true; }
const task = await flowdex.createTask({
  name: 'task-lifecycle',
  instructions: 'Update task.txt and commit the changes.',
  readScope: ['task.txt'],
  writeScope: ['task.txt'],
  verification: ['git status --porcelain'],
});
let runUnknownRejected = false;
try {
  await task.runAgent({ name: 'invalid', instructions: 'invalid', model: 'gpt-5.4', extra: true });
} catch { runUnknownRejected = true; }
const initial = await task.runAgent({ name: 'task_worker', instructions: 'Make the initial change.', model: 'gpt-5.4', reasoningEffort: 'high' });
const firstVerification = await task.verify();
const resumed = await flowdex.resumeAgent(initial.agentId, 'Make a second change and commit it.', { contextMode: 'compact' });
let staleRejected = false;
try { await task.integrate(); } catch { staleRejected = true; }
const secondVerification = await task.verify();
const integrated = await task.integrate();
text(JSON.stringify({ taskId: task.id, createUnknownRejected, runUnknownRejected, initial, firstVerification, resumed, staleRejected, secondVerification, integrated }));"#,
            )?;
            fs::write(cwd.join("README.md"), "flowdex task fixture\n")?;
            run_git(&["init"])?;
            run_git(&["config", "user.name", "Flowdex Test"])?;
            run_git(&["config", "user.email", "flowdex-test@example.com"])?;
            run_git(&["add", "."])?;
            run_git(&["commit", "-m", "fixture baseline"])?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/task.js" });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-task-parent-1"),
            ev_function_call("call-task-1", "start_flowdex_workflow", &args.to_string()),
            ev_completed("resp-task-parent-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "Make the initial change")
                && !body_contains(request, "Make a second change")
        },
        sse(vec![
            ev_response_created("resp-task-child-1"),
            core_test_support::responses::ev_shell_command_call(
                "call-task-child-1",
                "echo first > task.txt && git add task.txt && git commit -m \"initial task change\"",
            ),
            ev_completed("resp-task-child-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-task-child-1"),
        sse(vec![
            ev_response_created("resp-task-child-1-followup"),
            ev_assistant_message("msg-task-child-1-followup", "initial task change committed"),
            ev_completed("resp-task-child-1-followup"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "initial task change committed")
                && body_contains(request, "compaction")
        },
        sse(vec![
            ev_response_created("resp-task-compact"),
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "TASK_COMPACTED_CONTEXT",
                }
            }),
            ev_completed("resp-task-compact"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "Make a second change"),
        sse(vec![
            ev_response_created("resp-task-child-2"),
            core_test_support::responses::ev_shell_command_call(
                "call-task-child-2",
                "echo second >> task.txt && git add task.txt && git commit -m \"resumed task change\"",
            ),
            ev_completed("resp-task-child-2"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-task-child-2"),
        sse(vec![
            ev_response_created("resp-task-child-2-followup"),
            ev_assistant_message("msg-task-child-2-followup", "resumed task change committed"),
            ev_completed("resp-task-child-2-followup"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-task-1"),
        sse(vec![
            ev_response_created("resp-task-parent-2"),
            ev_completed("resp-task-parent-2"),
        ]),
    )
    .await;

    test.submit_turn("run the task lifecycle workflow").await?;

    let output = follow_up
        .function_call_output_text("call-task-1")
        .expect("task workflow output should be returned");
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "completed");
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    let task_id = workflow_output["taskId"].as_str().expect("task id");
    let agent_id = workflow_output["initial"]["agentId"]
        .as_str()
        .expect("initial agent id");
    assert_eq!(workflow_output["initial"]["status"], "completed");
    assert_eq!(workflow_output["createUnknownRejected"], true);
    assert_eq!(workflow_output["runUnknownRejected"], true);
    assert_eq!(workflow_output["resumed"]["status"], "completed");
    assert_eq!(workflow_output["resumed"]["agentId"], agent_id);
    assert_eq!(workflow_output["firstVerification"]["passed"], true);
    assert_eq!(workflow_output["secondVerification"]["passed"], true);
    assert_eq!(workflow_output["staleRejected"], true);

    let commits = workflow_output["integrated"]["commits"]
        .as_array()
        .expect("integrated commits");
    assert_eq!(workflow_output["integrated"]["taskId"], task_id);
    assert_eq!(commits.len(), 2);
    for commit in commits {
        assert_eq!(commit["agentId"], agent_id);
        assert_eq!(commit["model"], "gpt-5.4");
        assert!(
            commit["sourceCommit"]
                .as_str()
                .is_some_and(|hash| hash.len() == 40)
        );
        assert!(
            commit["integratedCommit"]
                .as_str()
                .is_some_and(|hash| hash.len() == 40)
        );
    }
    assert!(
        commits[0]["summary"]
            .as_str()
            .unwrap()
            .contains("initial task change")
    );
    assert!(
        commits[1]["summary"]
            .as_str()
            .unwrap()
            .contains("resumed task change")
    );
    assert!(commits[0]["sourceCommit"] != commits[0]["integratedCommit"]);
    assert!(commits[1]["sourceCommit"] != commits[1]["integratedCommit"]);

    let content = fs::read_to_string(test.workspace_path("task.txt"))?;
    assert!(content.contains("first"));
    assert!(content.contains("second"));
    let flowdex_root = test.codex_home_path().join("flowdex").join("worktrees");
    assert!(!flowdex_contains_file(&flowdex_root, "task.txt"));
            Ok(())
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flowdex_scheduler_runs_parallel_dependencies_and_verification() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.features.enable(Feature::Collab).unwrap();
            config.agent_max_threads = Some(8);
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("scheduler.js"),
                r#"const run = await flowdex.startRun({
  name: 'scheduler-fixture',
  agents: { worker: { model: 'gpt-5.4' } },
  verification: ['git status --porcelain'],
  phases: [{
    name: 'build',
    instructions: 'Complete and commit the build task.',
    verification: ['git status --porcelain'],
    tasks: [
      { name: 'alpha', agent: 'worker', instructions: 'alpha instructions', verification: ['git status --porcelain'], writeScope: ['alpha.txt'] },
      { name: 'beta', agent: 'worker', instructions: 'beta instructions', verification: ['git status --porcelain'], writeScope: ['beta.txt'] },
      { name: 'join', agent: 'worker', instructions: 'join instructions', dependencies: ['alpha', 'beta'], verification: ['git status --porcelain'], writeScope: ['joined.txt'] },
    ],
  }],
});
text(JSON.stringify(await run.wait()));"#,
            )?;
            fs::write(cwd.join("README.md"), "scheduler fixture\n")?;
            let run_git = |args: &[&str]| -> Result<()> {
                let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                if !output.status.success() {
                    anyhow::bail!(
                        "git {args:?} failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Ok(())
            };
            run_git(&["init"])?;
            run_git(&["config", "user.name", "Flowdex Test"])?;
            run_git(&["config", "user.email", "flowdex-test@example.com"])?;
            run_git(&["add", "."])?;
            run_git(&["commit", "-m", "fixture baseline"])?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let agent_responder = SchedulerAgentResponder::default();
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(agent_responder.clone())
        .mount(&server)
        .await;

    let mut created = test.thread_manager.subscribe_thread_created();
    let mut reasoning = Vec::new();
    let submit = test.codex.submit(Op::UserInput {
        items: vec![UserInput::Text {
            text: "run the scheduler workflow".into(),
            text_elements: Vec::new(),
        }],
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: Default::default(),
    });
    submit.await?;
    let turn_id = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), test.codex.next_event())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "scheduler event timeout after {reasoning:?}; requests initial={}/{} follow={}/{}",
                    agent_responder.alpha_requests.load(Ordering::SeqCst),
                    agent_responder.beta_requests.load(Ordering::SeqCst),
                    0,
                    0,
                )
            })??;
        if let EventMsg::TurnStarted(event) = event.msg {
            break event.turn_id;
        }
    };
    let mut saw_parallel = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), test.codex.next_event())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "scheduler event timeout after {reasoning:?}; requests initial={}/{} follow={}/{}",
                    agent_responder.alpha_requests.load(Ordering::SeqCst),
                    agent_responder.beta_requests.load(Ordering::SeqCst),
                    0,
                    0,
                )
            })??;
        saw_parallel = saw_parallel
            || (agent_responder.alpha_requests.load(Ordering::SeqCst) == 1
                && agent_responder.beta_requests.load(Ordering::SeqCst) == 1);
        match event.msg {
            EventMsg::ItemStarted(ItemStartedEvent {
                item: TurnItem::Reasoning(item),
                ..
            }) => reasoning.extend(item.summary_text),
            EventMsg::TurnComplete(event) if event.turn_id == turn_id => break,
            _ => {}
        }
    }
    assert!(
        saw_parallel,
        "both independent task agents should be running before release"
    );

    let requests = server.received_requests().await.unwrap_or_default();
    let output_call = if requests
        .iter()
        .any(|request| has_function_call_output(request, "call-scheduler-wait"))
    {
        "call-scheduler-wait"
    } else {
        "call-scheduler-outer"
    };
    let follow_up_request = requests
        .iter()
        .find(|request| has_function_call_output(request, output_call))
        .expect("scheduler output should be returned");
    let output: Value = serde_json::from_str(
        &function_call_output_text(follow_up_request, output_call)
            .expect("scheduler output should be text"),
    )?;
    assert_eq!(output["status"], "completed", "scheduler output: {output}");
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["status"], "completed");

    let expected = [
        "Running workflow: scheduler-fixture",
        "Running phase 1/1: build",
        "Running task: alpha",
        "Running task: beta",
        "Verifying task: alpha",
        "Verifying task: beta",
        "Integrating task: alpha",
        "Integrating task: beta",
        "Running task: join",
        "Verifying task: join",
        "Integrating task: join",
        "Verifying phase 1/1: build",
        "Verifying workflow: scheduler-fixture",
        "Completed workflow: scheduler-fixture",
    ];
    for summary in expected {
        assert!(
            reasoning.iter().any(|item| item == summary),
            "missing summary: {summary}"
        );
    }
    let alpha_index = reasoning
        .iter()
        .position(|item| item == "Integrating task: alpha")
        .unwrap();
    let beta_index = reasoning
        .iter()
        .position(|item| item == "Integrating task: beta")
        .unwrap();
    assert!(
        alpha_index < beta_index,
        "integration should follow declaration order"
    );
    for summary in expected {
        assert!(!body_contains(follow_up_request, summary));
    }
    assert!(!body_contains(follow_up_request, "alpha complete"));
    assert!(!body_contains(follow_up_request, "beta complete"));

    for _ in 0..3 {
        let child_id = tokio::time::timeout(Duration::from_secs(5), created.recv()).await??;
        let snapshot = test
            .thread_manager
            .get_thread(child_id)
            .await?
            .config_snapshot()
            .await;
        assert!(matches!(
            snapshot.session_source,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
        ));
    }
    Ok(())
}

#[test]
fn flowdex_joined_saved_workflow_crosses_scheduler_boundaries() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.features.enable(Feature::Collab).unwrap();
            config.agent_max_threads = Some(10);
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("joined.js"),
                r#"const input = flowdex.requireInput({
  properties: { project: { type: 'string' } }, required: ['project'],
});
const run = await flowdex.startRun({
  name: 'joined-fixture',
  boundary: 'continue',
  agents: {
    worker: { model: 'gpt-5.4' },
    collector: { model: 'gpt-5.4' },
  },
  verification: ['git status --porcelain'],
  phases: [{
    name: 'build', instructions: 'Joined phase instructions.',
    verification: ['git status --porcelain'],
    tasks: [
      { name: 'alpha', agent: 'worker', instructions: 'alpha joined instructions', writeScope: ['alpha.txt'], verification: ['git status --porcelain'] },
      { name: 'beta', agent: 'worker', instructions: 'beta joined instructions', writeScope: ['beta.txt'], verification: ['git status --porcelain'] },
      { name: 'dependent', agent: 'worker', instructions: 'dependent joined instructions', dependencies: ['alpha', 'beta'], writeScope: ['dependent.txt'], verification: ['git status --porcelain'] },
      { name: 'later', agent: 'worker', instructions: 'later joined instructions', dependencies: ['dependent'], writeScope: ['later.txt'], verification: ['git status --porcelain'] },
    ],
  }],
});
const result = await run.wait();
text(JSON.stringify({ input, result }));"#,
            )?;
            fs::write(cwd.join("README.md"), "joined flowdex fixture\n")?;
            let git = |args: &[&str]| -> Result<()> {
                let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                anyhow::ensure!(output.status.success(), "git {args:?} failed");
                Ok(())
            };
            git(&["init"])?;
            git(&["config", "user.name", "Flowdex Test"])?;
            git(&["config", "user.email", "flowdex-test@example.com"])?;
            git(&["add", "."])?;
            git(&["commit", "-m", "joined fixture baseline"])?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let responder = JoinedFlowResponder::default();
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(responder.clone())
        .mount(&server)
        .await;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "run the joined workflow".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    let turn_id = loop {
        let event =
            tokio::time::timeout(Duration::from_secs(30), test.codex.next_event()).await??;
        if let EventMsg::TurnStarted(event) = event.msg {
            break event.turn_id;
        }
    };
    let mut reasoning = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), test.codex.next_event())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "joined event timeout: alpha={} beta={} parent_steps={} reasoning={reasoning:?}",
                    responder.alpha_requests.load(Ordering::SeqCst),
                    responder.beta_requests.load(Ordering::SeqCst),
                    responder.parent_steps.load(Ordering::SeqCst),
                )
            })??;
        match event.msg {
            EventMsg::ItemStarted(ItemStartedEvent {
                item: TurnItem::Reasoning(item),
                ..
            }) => reasoning.extend(item.summary_text),
            EventMsg::TurnComplete(event) if event.turn_id == turn_id => break,
            _ => {}
        }
    }
    assert_eq!(responder.alpha_requests.load(Ordering::SeqCst), 1);
    assert_eq!(responder.beta_requests.load(Ordering::SeqCst), 1);
    assert!(
        reasoning
            .iter()
            .any(|summary| summary == "Running task: alpha")
    );
    assert!(
        reasoning
            .iter()
            .any(|summary| summary == "Running task: beta")
    );
    assert!(
        reasoning
            .iter()
            .any(|summary| summary == "Running task: later")
    );

    let requests = server.received_requests().await.unwrap_or_default();
    let dependent = requests
        .iter()
        .find(|request| body_contains(request, "dependent joined instructions"))
        .expect("dependent task request");
    assert!(body_contains(dependent, "Joined phase instructions."));
    let follow_up = requests
        .iter()
        .find(|request| has_function_call_output(request, "call-joined-start"))
        .expect("start output should return to parent");
    assert!(!body_contains(follow_up, "Running task: alpha"));
    assert!(!body_contains(follow_up, "task complete with no changes"));

    let mut child_count = 0;
    while child_count < 4 {
        match tokio::time::timeout(Duration::from_secs(10), created.recv()).await {
            Ok(Ok(_)) => child_count += 1,
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => {
                anyhow::bail!("timed out waiting for app-visible child agents ({child_count})")
            }
        }
    }
        Ok(())
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flowdex_context_pack_collects_stale_and_reinjects() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.features.enable(Feature::Collab).unwrap();
            config.agent_max_threads = Some(8);
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        })
        .with_workspace_setup(|cwd, _fs| async move {
            fs::create_dir_all(cwd.join(".flowdex/workflows"))?;
            fs::write(cwd.join("context.txt"), "OLD\n")?;
            fs::write(
                cwd.join(".flowdex/workflows/context.js"),
                r#"const run = await flowdex.startRun({
  name: 'context-fixture', agents: { explorer: { model: 'gpt-5.4' }, worker: { model: 'gpt-5.4' } },
  contextPacks: { fixture: { agent: 'explorer', instructions: 'Collect context.' } },
  phases: [{ name: 'build', instructions: 'Build.', tasks: [
    { name: 'first', agent: 'worker', instructions: 'first instructions', context: ['fixture'] },
    { name: 'independent', agent: 'worker', instructions: 'independent instructions' },
    { name: 'second', agent: 'worker', instructions: 'second instructions', dependencies: ['first', 'independent'], context: ['fixture'] },
  ] }],
}); text(JSON.stringify(await run.wait()));"#,
            )?;
            let git = |args: &[&str]| -> Result<()> {
                let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                if !output.status.success() { anyhow::bail!("git {args:?} failed"); }
                Ok(())
            };
            git(&["init"])?;
            git(&["config", "core.autocrlf", "false"])?;
            git(&["config", "user.name", "Flowdex Test"])?;
            git(&["config", "user.email", "flowdex-test@example.com"])?;
            git(&["add", "."])?; git(&["commit", "-m", "fixture baseline"])?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let responder = ContextPackResponder::default();
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(responder.clone())
        .mount(&server)
        .await;
    let first_requests = Arc::clone(&responder.first_requests);
    let context_path = test.workspace_path("context.txt");
    let workspace_path = test.workspace_path(".");
    let modifier = tokio::spawn(async move {
        while first_requests.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        fs::write(context_path, "NEW\n")?;
        for args in [
            &["add", "context.txt"][..],
            &["commit", "-m", "refresh context"][..],
        ] {
            let output = Command::new("git")
                .current_dir(&workspace_path)
                .args(args)
                .output()?;
            if !output.status.success() {
                anyhow::bail!("git {args:?} failed");
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "run the context workflow".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    let turn_id = loop {
        let event =
            tokio::time::timeout(Duration::from_secs(30), test.codex.next_event()).await??;
        if let EventMsg::TurnStarted(event) = event.msg {
            break event.turn_id;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), modifier).await???;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timed out waiting for Flowdex context workflow");
        }
        let event = match tokio::time::timeout(remaining, test.codex.next_event()).await {
            Ok(result) => result?,
            Err(_) => anyhow::bail!("timed out waiting for Flowdex context workflow"),
        };
        if let EventMsg::TurnComplete(event) = event.msg
            && event.turn_id == turn_id
        {
            break;
        }
    }
    assert_eq!(
        responder.collector_requests.load(Ordering::SeqCst),
        2,
        "requests: independent={}, first={}, second={}",
        responder.independent_requests.load(Ordering::SeqCst),
        responder.first_requests.load(Ordering::SeqCst),
        responder.second_requests.load(Ordering::SeqCst),
    );
    assert_eq!(responder.independent_requests.load(Ordering::SeqCst), 1);
    assert!(
        responder
            .independent_started_before_collector
            .load(Ordering::SeqCst)
    );
    assert_eq!(responder.first_requests.load(Ordering::SeqCst), 1);
    assert_eq!(responder.second_requests.load(Ordering::SeqCst), 1);
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.iter().any(|request| body_contains(request, "OLD")));
    assert!(requests.iter().any(|request| body_contains(request, "NEW")));
    assert!(
        requests
            .iter()
            .any(|request| body_contains(request, "context: context.txt"))
    );
    let flowdex_root = test.codex_home_path().join("flowdex").join("worktrees");
    assert!(!flowdex_contains_file(&flowdex_root, "context.txt"));
    let parent_requests = requests
        .iter()
        .filter(|request| body_contains(request, "run the context workflow"));
    for request in parent_requests {
        assert!(!input_contains(request, "OLD"));
        assert!(!input_contains(request, "NEW"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flowdex_nested_workflow_runs_scheduler_child_without_parent_model_turn() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.features.enable(Feature::Collab).unwrap();
            config.agent_max_threads = Some(4);
            config.active_project.trust_level = Some(TrustLevel::Trusted);
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("parent.js"),
                r#"const input = flowdex.requireInput({
  properties: { runChild: { type: 'boolean' }, files: { type: 'array', items: { type: 'string' } } },
  required: ['runChild', 'files'],
});
let child = null;
if (input.runChild) child = await flowdex.runWorkflow('repo:child', { files: input.files });
flowdex.output({ child });"#,
            )?;
            fs::write(
                workflow_dir.join("child.js"),
                r#"const input = flowdex.requireInput({
  properties: { files: { type: 'array', items: { type: 'string' } } },
  required: ['files'],
});
flowdex.output({ input, workflow: flowdex.workflowPath });"#,
            )?;
            fs::write(cwd.join("README.md"), "nested workflow fixture\n")?;
            let run_git = |args: &[&str]| -> Result<()> {
                let output = Command::new("git").current_dir(&cwd).args(args).output()?;
                if !output.status.success() {
                    anyhow::bail!(
                        "git {args:?} failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Ok(())
            };
            run_git(&["init"])?;
            run_git(&["config", "user.name", "Flowdex Test"])?;
            run_git(&["config", "user.email", "flowdex-test@example.com"])?;
            run_git(&["add", "."])?;
            run_git(&["commit", "-m", "fixture baseline"])?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build(&server).await?;
    let responder = NestedWorkflowResponder::default();
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(responder.clone())
        .mount(&server)
        .await;
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "run nested workflow".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let turn_id = loop {
        let event =
            tokio::time::timeout(Duration::from_secs(30), test.codex.next_event()).await??;
        if let EventMsg::TurnStarted(event) = event.msg {
            break event.turn_id;
        }
    };
    loop {
        let event =
            tokio::time::timeout(Duration::from_secs(30), test.codex.next_event()).await??;
        match event.msg {
            EventMsg::TurnComplete(event) if event.turn_id == turn_id => break,
            _ => {}
        }
    }

    let requests = server.received_requests().await.unwrap_or_default();
    eprintln!(
        "parent requests={}, total requests={}",
        responder.parent_requests.load(Ordering::SeqCst),
        requests.len(),
    );
    let follow_up_request = requests
        .iter()
        .find(|request| has_function_call_output(request, "call-nested-parent"))
        .expect("nested parent output should be returned");
    let output: Value = serde_json::from_str(
        &function_call_output_text(follow_up_request, "call-nested-parent")
            .expect("nested parent output should be text"),
    )?;
    assert_eq!(output["status"], "completed", "nested output: {output}");
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    let child = &workflow_output["child"];
    assert_eq!(
        child["input"]["files"],
        serde_json::json!(["child.txt"]),
        "workflow output: {workflow_output}"
    );
    assert_eq!(child["workflow"], "repo:child");
    assert_eq!(
        responder.parent_requests.load(Ordering::SeqCst),
        2,
        "nested execution must not add an intermediate model request"
    );
    Ok(())
}
