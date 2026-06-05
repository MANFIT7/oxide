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

    let mut config = Config::default();
    config.provider = "mock".into();
    config.approval_policy = oxide_protocol::ApprovalPolicy::Never; // auto-run tools
    config.workspace = Some(tmp.clone());

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
async fn mock_provider_browser_tool_emits_browser_events() {
    let tmp = std::env::temp_dir().join(format!("oxide-browser-event-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let mut config = Config::default();
    config.provider = "mock_browser".into();
    config.approval_policy = oxide_protocol::ApprovalPolicy::Never;
    config.persist = false;
    config.workspace = Some(tmp.clone());

    let (handle, mut events) = oxide_core::spawn(config).expect("spawn");
    let _ = events.recv().await; // Ready

    handle
        .submit(Op::UserTurn {
            text: "open browser".into(),
        })
        .await
        .unwrap();

    let mut target_seen = false;
    let mut snapshot_seen = false;
    while let Some(ev) = events.recv().await {
        match ev {
            Event::BrowserTargetChanged { url, note, .. } => {
                target_seen = url == "http://localhost:3000" && note == "Open login page";
            }
            Event::BrowserSnapshotRequested { url, note, .. } => {
                snapshot_seen = url == "http://localhost:3000" && note == "Capture login page";
            }
            Event::TurnFinished { .. } => break,
            _ => {}
        }
    }

    assert!(target_seen, "browser target event should be emitted");
    assert!(snapshot_seen, "browser snapshot event should be emitted");
    handle.submit(Op::Shutdown).await.unwrap();
    std::fs::remove_dir_all(tmp).ok();
}

#[tokio::test]
async fn checkpoint_then_rewind_undoes_write() {
    let tmp = std::env::temp_dir().join(format!("oxide-rewind-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    let mut config = Config::default();
    config.provider = "mock".into();
    config.approval_policy = oxide_protocol::ApprovalPolicy::Never;
    config.persist = false; // keep the test workspace clean
    config.workspace = Some(tmp.clone());

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
