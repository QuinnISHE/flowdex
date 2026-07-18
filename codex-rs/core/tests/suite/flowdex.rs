use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use serde_json::Value;
use std::fs;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_flowdex_workflow_executes_saved_v8_module() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
        })
        .with_workspace_setup(|cwd, _fs| async move {
            let workflow_dir = cwd.join(".flowdex/workflows");
            fs::create_dir_all(&workflow_dir)?;
            fs::write(
                workflow_dir.join("hello.js"),
                "await Promise.resolve(); text(JSON.stringify({ input: flowdex.input, path: flowdex.workflowPath }));",
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
    assert!(output.get("error").is_none());
    Ok(())
}
