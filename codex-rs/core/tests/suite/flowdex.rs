use anyhow::Result;
use codex_features::Feature;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::Op;
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
        |request: &wiremock::Request| body_contains(request, "child instructions"),
        sse(vec![
            ev_response_created("resp-child-1"),
            ev_assistant_message("msg-child-1", "child output"),
            ev_completed("resp-child-1"),
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
