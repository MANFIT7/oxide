//! End-to-end smoke: drive the engine purely via Op/Event (no UI), proving the
//! loop is frontend-agnostic and the echo provider streams a full turn.

use oxide_config::Config;
use oxide_protocol::{Event, Op};

#[tokio::test]
async fn echo_turn_streams_and_finishes() {
    let config = Config::default(); // harness=default, provider=echo
    let (handle, mut events) = oxide_core::spawn(config).expect("spawn engine");

    // Wait for Ready.
    match events.recv().await {
        Some(Event::Ready { harness }) => assert_eq!(harness, "default"),
        other => panic!("expected Ready, got {other:?}"),
    }

    handle
        .submit(Op::UserTurn {
            text: "halo oxide".into(),
        })
        .await
        .unwrap();

    let mut started = false;
    let mut reply = String::new();
    let mut finished = false;

    while let Some(ev) = events.recv().await {
        match ev {
            Event::TurnStarted { .. } => started = true,
            Event::AgentMessageDelta { text, .. } => reply.push_str(&text),
            Event::TurnFinished { .. } => {
                finished = true;
                break;
            }
            _ => {}
        }
    }

    assert!(started, "turn should start");
    assert!(finished, "turn should finish");
    assert!(
        reply.contains("halo oxide"),
        "echo should include input, got: {reply:?}"
    );

    handle.submit(Op::Shutdown).await.unwrap();
}

#[tokio::test]
async fn mock_provider_tool_call_writes_file_in_sandbox() {
    let tmp = std::env::temp_dir().join(format!("oxide-engine-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let config = Config {
        provider: "mock".into(),
        approval_policy: oxide_protocol::ApprovalPolicy::Never,
        workspace: Some(tmp.clone()),
        ..Default::default()
    };

    let (handle, mut events) = oxide_core::spawn(config).expect("spawn");
    let _ = events.recv().await; // Ready

    handle
        .submit(Op::UserTurn {
            text: "make a file".into(),
        })
        .await
        .unwrap();

    let mut began = false;
    let mut patched = false;
    let mut ended_ok = false;
    while let Some(ev) = events.recv().await {
        match ev {
            Event::ToolCallBegin { tool, .. } => began = tool == "write_file",
            Event::PatchApplied { path, .. } => patched = path == "oxide_mock.txt",
            Event::ToolCallEnd { ok, .. } => ended_ok = ok,
            Event::TurnFinished { .. } => break,
            _ => {}
        }
    }

    assert!(began, "tool call should begin");
    assert!(patched, "PatchApplied should fire for the write");
    assert!(ended_ok, "tool should execute ok");
    assert!(
        tmp.join("oxide_mock.txt").exists(),
        "file should exist in sandbox workspace"
    );

    handle.submit(Op::Shutdown).await.unwrap();
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn mock_provider_renders_ui_spec_artifact() {
    let tmp = std::env::temp_dir().join(format!("oxide-engine-ui-spec-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let config = Config {
        provider: "mock".into(),
        approval_policy: oxide_protocol::ApprovalPolicy::Never,
        persist: false,
        workspace: Some(tmp.clone()),
        ..Default::default()
    };

    let (handle, mut events) = oxide_core::spawn(config).expect("spawn");
    let _ = events.recv().await; // Ready

    handle
        .submit(Op::UserTurn {
            text: "render ui spec artifact".into(),
        })
        .await
        .unwrap();

    let mut began = false;
    let mut rendered = false;
    let mut ended_ok = false;
    while let Some(ev) = events.recv().await {
        match ev {
            Event::ToolCallBegin { tool, .. } => began |= tool == "render_ui_spec",
            Event::UiSpec { spec, .. } => {
                assert_eq!(spec.title.as_deref(), Some("Mock UI"));
                spec.validate().expect("mock UI spec should validate");
                rendered = true;
            }
            Event::ToolCallEnd { ok, .. } => ended_ok |= ok,
            Event::TurnFinished { .. } => break,
            _ => {}
        }
    }

    assert!(began, "render_ui_spec tool call should begin");
    assert!(rendered, "engine should emit a UiSpec event");
    assert!(ended_ok, "render_ui_spec should finish ok");

    handle.submit(Op::Shutdown).await.unwrap();
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn orchestrated_subagents_run_backend_tool_calls() {
    let tmp = std::env::temp_dir().join(format!("oxide-subagents-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let config = Config {
        provider: "echo".into(),
        orchestrate: true,
        subagents: true,
        front_provider: "mock_plan".into(),
        backend_provider: "mock".into(),
        approval_policy: oxide_protocol::ApprovalPolicy::Never,
        persist: false,
        workspace: Some(tmp.clone()),
        ..Default::default()
    };

    let (handle, mut events) = oxide_core::spawn(config).expect("spawn");
    let _ = events.recv().await; // Ready

    handle
        .submit(Op::UserTurn {
            text: "make a file through subagents".into(),
        })
        .await
        .unwrap();

    let mut began = false;
    let mut patched = false;
    let mut ended_ok = false;
    let mut active_subagents = std::collections::HashSet::new();
    let mut started_subagents = 0usize;
    let mut max_parallel_subagents = 0usize;
    while let Some(ev) = events.recv().await {
        match ev {
            Event::SubagentStarted { worker_id, .. } if worker_id.starts_with("subagent-") => {
                started_subagents += 1;
                active_subagents.insert(worker_id);
                max_parallel_subagents = max_parallel_subagents.max(active_subagents.len());
            }
            Event::SubagentFinished { worker_id, .. } if worker_id.starts_with("subagent-") => {
                active_subagents.remove(&worker_id);
            }
            Event::ToolCallBegin { tool, .. } => began |= tool == "write_file",
            Event::PatchApplied { path, .. } => patched |= path == "oxide_mock.txt",
            Event::ToolCallEnd { ok, .. } => ended_ok |= ok,
            Event::TurnFinished { .. } => break,
            _ => {}
        }
    }

    assert!(began, "sub-agent backend should begin a tool call");
    assert!(
        started_subagents >= 2,
        "mock plan should fan out to at least two sub-agents"
    );
    assert!(
        max_parallel_subagents >= 2,
        "sub-agents should overlap instead of running sequentially"
    );
    assert!(patched, "sub-agent backend write should emit PatchApplied");
    assert!(ended_ok, "sub-agent backend tool should finish ok");
    assert!(
        tmp.join("oxide_mock.txt").exists(),
        "sub-agent backend should write in the workspace"
    );

    handle.submit(Op::Shutdown).await.unwrap();
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn checkpoint_then_rewind_undoes_write() {
    let tmp = std::env::temp_dir().join(format!("oxide-rewind-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let config = Config {
        provider: "mock".into(),
        approval_policy: oxide_protocol::ApprovalPolicy::Never,
        persist: false,
        workspace: Some(tmp.clone()),
        ..Default::default()
    };

    let (handle, mut events) = oxide_core::spawn(config).expect("spawn");
    let _ = events.recv().await; // Ready

    handle
        .submit(Op::UserTurn {
            text: "write".into(),
        })
        .await
        .unwrap();

    let mut checkpoint_id = None;
    while let Some(ev) = events.recv().await {
        match ev {
            Event::CheckpointCreated { id, .. } => checkpoint_id = Some(id),
            Event::TurnFinished { .. } => break,
            _ => {}
        }
    }
    let id = checkpoint_id.expect("a checkpoint should be created for the write");
    assert!(tmp.join("oxide_mock.txt").exists(), "file written");

    handle
        .submit(Op::Rewind { checkpoint_id: id })
        .await
        .unwrap();
    let mut restored = 0;
    while let Some(ev) = events.recv().await {
        if let Event::RewindDone { restored: r, .. } = ev {
            restored = r;
            break;
        }
    }
    assert_eq!(restored, 1, "one file restored/removed");
    assert!(
        !tmp.join("oxide_mock.txt").exists(),
        "rewind should delete the new file"
    );

    handle.submit(Op::Shutdown).await.unwrap();
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn switching_harness_emits_event() {
    let (handle, mut events) = oxide_core::spawn(Config::default()).expect("spawn");
    let _ = events.recv().await; // Ready

    handle
        .submit(Op::SetHarness {
            id: "hermes".into(),
        })
        .await
        .unwrap();
    let mut switched = false;
    while let Some(ev) = events.recv().await {
        if let Event::HarnessChanged { id } = ev {
            assert_eq!(id, "hermes");
            switched = true;
            break;
        }
    }
    assert!(switched);
    handle.submit(Op::Shutdown).await.unwrap();
}
