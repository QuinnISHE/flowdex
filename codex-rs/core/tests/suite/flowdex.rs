use anyhow::Result;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::items::TurnItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
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
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use core_test_support::wait_for_event_with_timeout;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body).is_ok_and(|body| body.to_string().contains(text))
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
async fn flowdex_progress_is_transient_reasoning_summary() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::CodeMode).unwrap();
    });
    builder = builder.with_workspace_setup(|cwd, _fs| async move {
        let workflow_dir = cwd.join(".flowdex/workflows");
        fs::create_dir_all(&workflow_dir)?;
        fs::write(
            workflow_dir.join("progress.js"),
            "const result = await flowdex.progress('  flowdex-progress-secret  '); text(JSON.stringify({ resultType: typeof result, done: 'ok' }));",
        )?;
        Ok::<(), anyhow::Error>(())
    });
    let test = builder.build(&server).await?;
    let args = serde_json::json!({ "path": ".flowdex/workflows/progress.js" });
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-progress-1"),
            ev_function_call(
                "call-progress-1",
                "start_flowdex_workflow",
                &args.to_string(),
            ),
            ev_completed("resp-progress-1"),
        ]),
    )
    .await;
    let follow_up = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "call-progress-1"),
        sse(vec![
            ev_response_created("resp-progress-2"),
            ev_completed("resp-progress-2"),
        ]),
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "run the progress workflow".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let started = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::Reasoning(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    let completed = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::Reasoning(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    assert_eq!(started.id, completed.id);
    assert_eq!(started.summary_text, vec!["flowdex-progress-secret"]);
    wait_for_event_with_timeout(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(_)),
        tokio::time::Duration::from_secs(30),
    )
    .await;

    let output = follow_up
        .function_call_output_text("call-progress-1")
        .expect("start tool output should be sent back to the model");
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "completed");
    let workflow_output: Value = serde_json::from_str(output["output"].as_str().unwrap())?;
    assert_eq!(workflow_output["resultType"], "undefined");
    assert_eq!(workflow_output["done"], "ok");
    assert!(
        !follow_up
            .single_request()
            .body_contains_text("flowdex-progress-secret")
    );
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
                r#"const task = await flowdex.createTask({
  name: 'task-lifecycle',
  instructions: 'Update task.txt and commit the changes.',
  readScope: ['task.txt'],
  writeScope: ['task.txt'],
  verification: ['git status --porcelain'],
});
const initial = await task.runAgent({ name: 'task_worker', instructions: 'Make the initial change.', model: 'gpt-5.4' });
const firstVerification = await task.verify();
const resumed = await flowdex.resumeAgent(initial.agentId, 'Make a second change and commit it.');
let staleRejected = false;
try { await task.integrate(); } catch { staleRejected = true; }
const secondVerification = await task.verify();
const integrated = await task.integrate();
text(JSON.stringify({ taskId: task.id, initial, firstVerification, resumed, staleRejected, secondVerification, integrated }));"#,
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
