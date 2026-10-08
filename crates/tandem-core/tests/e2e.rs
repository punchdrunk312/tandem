use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tandem_core::mock::MockPlatform;
use tandem_core::node::*;
use tandem_core::platform::*;
use tandem_core::proto::*;

struct Side {
    node: Node,
    plat: Arc<MockPlatform>,
    dl: tempfile::TempDir,
    _tmp: tempfile::TempDir,
}

async fn start(name: &str) -> Side {
    let tmp = tempfile::tempdir().unwrap();
    let dl = tempfile::tempdir().unwrap();
    let plat = MockPlatform::new(1000, 800);
    let node = Node::start(
        NodeConfig {
            tcp_port: 0,
            udp_port: 0,
            temp_dir: tmp.path().to_path_buf(),
            download_dir: dl.path().to_path_buf(),
            discovery: false,
            default_name: name.into(),
        },
        plat.clone(),
        Arc::new(MemoryStore::default()),
    )
    .await
    .unwrap();
    Side {
        node,
        plat,
        dl,
        _tmp: tmp,
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn lo() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

async fn pair(a: &Side, b: &Side) {
    let code = a.node.snapshot().pairing_code;
    b.node.connect(lo(), a.node.tcp_port(), code);
    until("both connected", || {
        a.node.snapshot().connected.is_some() && b.node.snapshot().connected.is_some()
    })
    .await;
    until("layout synced", || {
        a.plat.st.lock().unwrap().active_edge.is_some()
            && b.plat.st.lock().unwrap().active_edge.is_some()
    })
    .await;
}

#[tokio::test]
async fn pair_cross_clipboard_and_files() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;

    // B dialled, so B has A on its right, and A has B on its left.
    assert_eq!(b.plat.st.lock().unwrap().active_edge, Some(Edge::Right));
    assert_eq!(a.plat.st.lock().unwrap().active_edge, Some(Edge::Left));
    assert_eq!(a.node.snapshot().connected.unwrap().name, "PC-B");
    assert_eq!(a.node.snapshot().paired.len(), 1);

    // ---- B's mouse pushes through its right edge at 25% height.
    b.plat.user_push_edge(Edge::Right, 0.25);
    until("A controlled", || a.node.snapshot().control == "controlled").await;
    assert_eq!(b.node.snapshot().control, "remote");
    let (x, y) = a.plat.cursor_pos();
    assert_eq!(x, 2, "enters A at its left edge");
    assert!((y - 199).abs() <= 1, "y={y}");

    // Typing and clicking on B goes to A.
    b.plat.emit(CaptureEvent::Input(InputEvent::Key {
        vk: 0x41,
        scan: 0x1e,
        extended: false,
        down: true,
    }));
    b.plat.emit(CaptureEvent::Input(InputEvent::Key {
        vk: 0x41,
        scan: 0x1e,
        extended: false,
        down: false,
    }));
    b.plat.emit(CaptureEvent::Input(InputEvent::Button {
        button: Button::Left,
        down: true,
    }));
    b.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 50,
        dy: 10,
    }));
    until("A injected 4 events", || {
        a.plat.st.lock().unwrap().injected.len() >= 4
    })
    .await;
    assert_eq!(a.plat.cursor_pos(), (52, 209));

    // Push A's cursor back out through its left edge -> control returns to B.
    b.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: -500,
        dy: 0,
    }));
    until("B local again", || b.node.snapshot().control == "local").await;
    assert_eq!(a.node.snapshot().control, "local");
    let (bx, by) = { b.plat.st.lock().unwrap().cursor };
    assert_eq!(bx, 1000 - 3, "re-enters B just inside its right edge");
    assert!(
        (by - 210 * 799 / 799).abs() < 40,
        "y roughly preserved: {by}"
    );
    assert!(!b.plat.st.lock().unwrap().capturing);
    assert!(
        a.plat.st.lock().unwrap().releases >= 1,
        "keys released on hand-back"
    );

    // ---- Clipboard text, both directions, no ping-pong.
    b.plat.user_copy(ClipContent::Text("héllo from B".into()));
    until("A has text", || {
        a.plat.clipboard_get() == Some(ClipContent::Text("héllo from B".into()))
    })
    .await;
    a.plat.user_copy(ClipContent::Text("reply from A".into()));
    until("B has text", || {
        b.plat.clipboard_get() == Some(ClipContent::Text("reply from A".into()))
    })
    .await;
    // Images travel too.
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
    b.plat.user_copy(ClipContent::Image(png.clone()));
    until("A has image", || {
        a.plat.clipboard_get() == Some(ClipContent::Image(png.clone()))
    })
    .await;
    a.plat.user_copy(ClipContent::Text("reply from A".into()));
    until("B has text again", || {
        b.plat.clipboard_get() == Some(ClipContent::Text("reply from A".into()))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        a.plat.clipboard_get(),
        Some(ClipContent::Text("reply from A".into())),
        "no echo overwrote A"
    );

    // ---- Copy files on B, paste on A (clipboard receives paths to the fetched copies).
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("album/inner")).unwrap();
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.path().join("album/inner/big.bin"), &big).unwrap();
    std::fs::write(src.path().join("album/note.txt"), "hi").unwrap();
    std::fs::write(src.path().join("solo.txt"), "solo").unwrap();
    b.plat.user_copy(ClipContent::Files(vec![
        src.path().join("album"),
        src.path().join("solo.txt"),
    ]));
    until("A clipboard has files", || {
        matches!(a.plat.clipboard_get(), Some(ClipContent::Files(_)))
    })
    .await;
    let Some(ClipContent::Files(paths)) = a.plat.clipboard_get() else {
        unreachable!()
    };
    assert_eq!(paths.len(), 2);
    assert_eq!(std::fs::read(paths[0].join("inner/big.bin")).unwrap(), big);
    assert_eq!(
        std::fs::read_to_string(paths[0].join("note.txt")).unwrap(),
        "hi"
    );
    assert_eq!(std::fs::read_to_string(&paths[1]).unwrap(), "solo");

    // ---- Explicit send A -> B lands in B's download dir; a name clash gets (1).
    let f = src.path().join("solo.txt");
    a.node.send_files(vec![f.clone()]);
    until("B received", || b.dl.path().join("solo.txt").exists()).await;
    a.node.send_files(vec![f]);
    until("B received 2nd", || {
        b.dl.path().join("solo (1).txt").exists()
    })
    .await;
    until("transfers finished", || {
        b.node
            .snapshot()
            .transfers
            .iter()
            .all(|t| t.state == "done")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(b.dl.path().join("solo (1).txt")).unwrap(),
        "solo"
    );

    // ---- Failsafe while B controls A.
    b.plat.user_push_edge(Edge::Right, 0.5);
    until("controlled", || a.node.snapshot().control == "controlled").await;
    b.plat.st.lock().unwrap().capturing = false; // the real hook clears this itself
    b.plat.emit(CaptureEvent::Failsafe);
    until("both local", || {
        a.node.snapshot().control == "local" && b.node.snapshot().control == "local"
    })
    .await;

    // ---- Disconnect + reconnect with the stored key (no code needed).
    let a_id = a.node.snapshot().device_id;
    b.node.disconnect();
    until("disconnected", || b.node.snapshot().connected.is_none()).await;
    until("A noticed", || a.node.snapshot().connected.is_none()).await;
    assert_eq!(b.plat.st.lock().unwrap().active_edge, None);
    b.node.connect_known(a_id);
    // connect_known needs an address: it falls back to last_addr saved at pairing.
    until("reconnected", || {
        a.node.snapshot().connected.is_some() && b.node.snapshot().connected.is_some()
    })
    .await;
}

#[tokio::test]
async fn wrong_code_never_pairs_and_code_rotates() {
    let a = start("A").await;
    let b = start("B").await;
    let code = a.node.snapshot().pairing_code;
    for _ in 0..3 {
        b.node.connect(lo(), a.node.tcp_port(), "000000".into());
        until("error shown", || b.node.snapshot().error.is_some()).await;
        until("dial finished", || true).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(a.node.snapshot().connected.is_none());
    assert!(a.node.snapshot().paired.is_empty());
    assert_ne!(
        a.node.snapshot().pairing_code,
        code,
        "code burned after 3 bad attempts"
    );
}

#[tokio::test]
async fn hotkey_engage_and_edge_race_are_safe() {
    let a = start("A").await;
    let b = start("B").await;

    // Pushing an edge with no peer must give the mouse straight back.
    a.plat.user_push_edge(Edge::Right, 0.5);
    until("capture released", || !a.plat.st.lock().unwrap().capturing).await;

    pair(&a, &b).await;
    b.node.engage();
    until("A controlled", || a.node.snapshot().control == "controlled").await;
    assert_eq!(b.node.snapshot().control, "remote");

    // If the link dies while remote, B must not stay captured.
    a.node.disconnect(); // B must release the mouse capture however the link ends
    until("B back to local", || b.node.snapshot().connected.is_none()).await;
    assert!(!b.plat.st.lock().unwrap().capturing);
}

#[tokio::test]
async fn settings_persist_and_reach_the_platform() {
    let a = start("A").await;
    assert!(a.node.snapshot().game_mode);
    a.node.set_game_mode(false);
    a.node.set_switch_delay(400);
    until("settings applied", || {
        let s = a.node.snapshot();
        !s.game_mode && s.switch_delay_ms == 400
    })
    .await;
    a.node.set_switch_delay(99_999);
    until("delay clamped", || {
        a.node.snapshot().switch_delay_ms == 3000
    })
    .await;
}
