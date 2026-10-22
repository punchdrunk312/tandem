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
    start_with(name, None).await
}

async fn start_with(name: &str, license_key: Option<[u8; 32]>) -> Side {
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
            license_key,
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
        // The listener first picks a default side, then adopts the opposite of the
        // dialler's announcement. Wait for the two to agree, not just to exist.
        let ea = a.plat.st.lock().unwrap().active_edge;
        let eb = b.plat.st.lock().unwrap().active_edge;
        matches!(
            (ea, eb),
            (Some(Edge::Left), Some(Edge::Right))
                | (Some(Edge::Right), Some(Edge::Left))
                | (Some(Edge::Top), Some(Edge::Bottom))
                | (Some(Edge::Bottom), Some(Edge::Top))
        )
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

// ------------------------------------------------------------------ several computers

fn edges_of(s: &Side) -> Vec<Edge> {
    let mut e = s.plat.st.lock().unwrap().active_edges.clone();
    e.sort_by_key(|x| format!("{x:?}"));
    e
}

fn connected_names(s: &Side) -> Vec<String> {
    let snap = s.node.snapshot();
    let mut v: Vec<String> = snap
        .paired
        .iter()
        .filter(|p| p.connected)
        .map(|p| p.name.clone())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn hub_with_two_computers_on_different_sides() {
    let a = start("HUB").await;
    let b = start("PC-B").await;
    let c = start("PC-C").await;
    pair(&a, &b).await;
    // The hub's pairing code rotates after each pairing: ask for the new one.
    let code = a.node.snapshot().pairing_code;
    c.node.connect(lo(), a.node.tcp_port(), code);
    until("hub sees both", || connected_names(&a).len() == 2).await;
    until("C laid out", || edges_of(&c) == vec![Edge::Left]).await;

    // Both dialled with the same default, but the hub keeps one computer per side.
    assert_eq!(edges_of(&a), vec![Edge::Left, Edge::Right]);
    assert_eq!(
        edges_of(&b),
        vec![Edge::Right],
        "B has the hub on its right"
    );
    let sides: Vec<_> = a.node.snapshot().paired.iter().map(|p| p.side).collect();
    assert_eq!(sides.len(), 2);
    assert_ne!(sides[0], sides[1]);
    // The spokes only know the hub.
    assert_eq!(b.node.snapshot().paired.len(), 1);
    assert_eq!(c.node.snapshot().paired.len(), 1);

    let b_id = b.node.snapshot().device_id;
    let c_id = c.node.snapshot().device_id;
    // C sits on the hub's right (the side the hub handed it), B on the left.
    let side_of = |id: &str| {
        a.node
            .snapshot()
            .paired
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .side
    };
    assert_eq!(side_of(&c_id), Edge::Right);
    assert_eq!(side_of(&b_id), Edge::Left);

    // ---- The hub's mouse goes through the right edge: only C is driven.
    a.plat.user_push_edge(Edge::Right, 0.5);
    until("C controlled", || c.node.snapshot().control == "controlled").await;
    assert_eq!(a.node.snapshot().control, "remote");
    assert_eq!(a.node.snapshot().control_peer, Some(c_id.clone()));
    assert_eq!(b.node.snapshot().control, "local");
    a.plat
        .emit(CaptureEvent::Input(InputEvent::MouseMove { dx: 40, dy: 0 }));
    until("C injected", || {
        !c.plat.st.lock().unwrap().injected.is_empty()
    })
    .await;
    assert!(
        b.plat.st.lock().unwrap().injected.is_empty(),
        "B must not hear it"
    );
    // Back out of C's left edge: the hub has its mouse again.
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: -500,
        dy: 0,
    }));
    until("hub local", || a.node.snapshot().control == "local").await;
    until("C local", || c.node.snapshot().control == "local").await;

    // ---- And through the left edge: now B.
    a.plat.user_push_edge(Edge::Left, 0.5);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    assert_eq!(c.node.snapshot().control, "local");
    a.plat.st.lock().unwrap().capturing = false;
    a.plat.emit(CaptureEvent::Failsafe);
    until("B local", || b.node.snapshot().control == "local").await;

    // ---- Jump to a named computer without touching the edge.
    a.node.engage_peer(c_id.clone());
    until("C driven by jump", || {
        c.node.snapshot().control == "controlled"
    })
    .await;
    // If that computer disconnects, the hub gets its mouse back at once.
    c.node.disconnect();
    until("hub local after C left", || {
        a.node.snapshot().control == "local"
    })
    .await;
    assert!(!a.plat.st.lock().unwrap().capturing);
    until("hub keeps B", || {
        connected_names(&a) == vec!["PC-B".to_string()]
    })
    .await;
    assert_eq!(edges_of(&a), vec![Edge::Left]);
    assert_eq!(a.node.snapshot().connected.unwrap().name, "PC-B");

    // ---- C comes back by key, no code.
    let hub_id = a.node.snapshot().device_id;
    c.node.connect_known(hub_id);
    until("C back", || connected_names(&a).len() == 2).await;
}

#[tokio::test]
async fn clipboard_and_files_reach_every_computer() {
    let a = start("HUB").await;
    let b = start("PC-B").await;
    let c = start("PC-C").await;
    pair(&a, &b).await;
    let code = a.node.snapshot().pairing_code;
    c.node.connect(lo(), a.node.tcp_port(), code);
    until("hub sees both", || connected_names(&a).len() == 2).await;
    until("C laid out", || edges_of(&c) == vec![Edge::Left]).await;
    let clip = |s: &Side| s.plat.st.lock().unwrap().clipboard.clone();

    // Hub copies: both spokes get it.
    a.plat.user_copy(ClipContent::Text("from hub".into()));
    until("spokes got hub text", || {
        clip(&b) == Some(ClipContent::Text("from hub".into()))
            && clip(&c) == Some(ClipContent::Text("from hub".into()))
    })
    .await;

    // A spoke copies: the hub gets it and passes it on to the other spoke.
    b.plat.user_copy(ClipContent::Text("from B".into()));
    until("hub and C got B's text", || {
        clip(&a) == Some(ClipContent::Text("from B".into()))
            && clip(&c) == Some(ClipContent::Text("from B".into()))
    })
    .await;

    // Images travel the same way.
    c.plat
        .user_copy(ClipContent::Image(vec![137, 80, 78, 71, 1, 2, 3]));
    until("image everywhere", || {
        clip(&a) == Some(ClipContent::Image(vec![137, 80, 78, 71, 1, 2, 3]))
            && clip(&b) == Some(ClipContent::Image(vec![137, 80, 78, 71, 1, 2, 3]))
    })
    .await;

    // Files sent to one named computer reach only that one.
    let dir = tempfile::tempdir().unwrap();
    let f1 = dir.path().join("only-b.txt");
    std::fs::write(&f1, "b").unwrap();
    let b_id = b.node.snapshot().device_id;
    a.node.send_files_to(vec![f1], b_id);
    until("B has it", || b.dl.path().join("only-b.txt").exists()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!c.dl.path().join("only-b.txt").exists());

    // With nobody under control, a plain send goes to everyone.
    let f2 = dir.path().join("both.txt");
    std::fs::write(&f2, "both").unwrap();
    a.node.send_files(vec![f2]);
    until("both have it", || {
        b.dl.path().join("both.txt").exists() && c.dl.path().join("both.txt").exists()
    })
    .await;
    until("transfers finished", || {
        a.node
            .snapshot()
            .transfers
            .iter()
            .all(|t| t.state == "done")
    })
    .await;
    // Each transfer names the computer it went to.
    let mut peers: Vec<String> = a
        .node
        .snapshot()
        .transfers
        .iter()
        .map(|t| t.peer.clone())
        .collect();
    peers.sort();
    assert_eq!(peers, vec!["PC-B", "PC-B", "PC-C"]);
}

#[tokio::test]
async fn a_ninth_computer_is_turned_away() {
    let hub = start("HUB").await;
    let mut spokes = Vec::new();
    for i in 0..8 {
        let s = start(&format!("S{i}")).await;
        let code = hub.node.snapshot().pairing_code;
        s.node.connect(lo(), hub.node.tcp_port(), code);
        let want = i + 1;
        until("spoke joined", || connected_names(&hub).len() == want).await;
        spokes.push(s);
    }
    let sides: std::collections::HashSet<_> =
        hub.node.snapshot().paired.iter().map(|p| p.side).collect();
    assert_eq!(sides.len(), 8, "one computer per side and corner");
    let late = start("LATE").await;
    let code = hub.node.snapshot().pairing_code;
    late.node.connect(lo(), hub.node.tcp_port(), code);
    until("late told off", || hub.node.snapshot().error.is_some()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(connected_names(&hub).len(), 8);
}

#[tokio::test]
async fn a_computer_in_the_corner_is_reached_diagonally() {
    let a = start("MAIN").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    let id = a.node.snapshot().paired[0].id.clone();

    // Put B diagonally: up and to the right of A.
    a.node.set_side(id, Edge::TopRight);
    until("corner layout on both sides", || {
        a.plat.st.lock().unwrap().active_edge == Some(Edge::TopRight)
            && b.plat.st.lock().unwrap().active_edge == Some(Edge::BottomLeft)
    })
    .await;

    // A's mouse pushes into its top-right corner.
    a.plat.user_push_edge(Edge::TopRight, 0.5);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    assert_eq!(a.node.snapshot().control, "remote");
    let (x, y) = b.plat.cursor_pos();
    assert_eq!((x, y), (2, 797), "enters B in its bottom-left corner");

    // Going back means pushing into that same corner again, not just touching an edge.
    a.plat
        .emit(CaptureEvent::Input(InputEvent::MouseMove { dx: 0, dy: 10 }));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        b.node.snapshot().control,
        "controlled",
        "bottom edge alone is not the way back"
    );
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: -10,
        dy: 10,
    }));
    until("A back in control", || a.node.snapshot().control == "local").await;
    assert_eq!(b.node.snapshot().control, "local");
    let (x, y) = a.plat.cursor_pos();
    assert_eq!((x, y), (997, 2), "back at A's top-right corner");
}

#[tokio::test]
async fn a_protected_screen_hands_control_back() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;

    // B drives A, then Windows on A switches to the admin-prompt screen.
    b.plat.user_push_edge(Edge::Right, 0.5);
    until("A controlled", || a.node.snapshot().control == "controlled").await;
    a.plat.emit(CaptureEvent::InputBlocked);
    until("A is local again", || a.node.snapshot().control == "local").await;
    until("B got its mouse back", || {
        b.node.snapshot().control == "local"
    })
    .await;
    assert!(
        a.node.snapshot().notice.is_some(),
        "A explains what happened"
    );

    // And the other way round: B is driving, B's own screen goes protected.
    b.plat.user_push_edge(Edge::Right, 0.5);
    until("A controlled again", || {
        a.node.snapshot().control == "controlled"
    })
    .await;
    b.plat.emit(CaptureEvent::InputBlocked);
    until("B local", || b.node.snapshot().control == "local").await;
    until("A local", || a.node.snapshot().control == "local").await;
}

fn release_left(from: &Side) {
    from.plat.emit(CaptureEvent::Input(InputEvent::Button {
        button: Button::Left,
        down: false,
    }));
}

fn make_file(name: &str, body: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join(name);
    std::fs::write(&p, body).unwrap();
    (d, p)
}

#[tokio::test]
async fn a_dragged_file_is_dropped_where_the_mouse_is_let_go() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    let target = tempfile::tempdir().unwrap();
    b.plat.st.lock().unwrap().drop_dir = Some(target.path().to_path_buf());
    let (_keep, file) = make_file("report.txt", b"quarterly numbers");

    // A has B on its left: drag the file to the left edge.
    a.plat.user_drag_to_edge(Edge::Left, 0.4, vec![file]);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    // Wait until everything has arrived at B, then let go of the button.
    until("file arrived at B", || {
        std::fs::read_dir(b._tmp.path())
            .map(|d| {
                d.flatten().any(|e| {
                    e.file_name().to_string_lossy().starts_with("carry-")
                        && std::fs::read_dir(e.path())
                            .map(|mut x| x.next().is_some())
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
    .await;
    // Not dropped yet: the button is still down.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(std::fs::read_dir(target.path()).unwrap().next().is_none());

    release_left(&a);
    until("file dropped", || target.path().join("report.txt").exists()).await;
    assert_eq!(
        std::fs::read(target.path().join("report.txt")).unwrap(),
        b"quarterly numbers"
    );
    until("transfer done", || {
        b.node
            .snapshot()
            .transfers
            .iter()
            .any(|t| t.state == "done" && t.path.as_deref() == target.path().to_str())
    })
    .await;
    // The staging folder is gone.
    assert!(std::fs::read_dir(b._tmp.path())
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with("carry-")));
}

#[tokio::test]
async fn letting_go_before_the_files_arrive_still_drops_them() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    let target = tempfile::tempdir().unwrap();
    b.plat.st.lock().unwrap().drop_dir = Some(target.path().to_path_buf());
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("photos/2026")).unwrap();
    std::fs::write(dir.path().join("photos/2026/a.jpg"), vec![7u8; 300_000]).unwrap();
    std::fs::write(dir.path().join("photos/b.jpg"), b"bb").unwrap();

    a.plat
        .user_drag_to_edge(Edge::Left, 0.5, vec![dir.path().join("photos")]);
    // Release at once: the button-up can overtake the offer.
    release_left(&a);
    until("folder dropped", || {
        target.path().join("photos/2026/a.jpg").exists()
            && target.path().join("photos/b.jpg").exists()
    })
    .await;
    assert_eq!(
        std::fs::metadata(target.path().join("photos/2026/a.jpg"))
            .unwrap()
            .len(),
        300_000
    );
}

#[tokio::test]
async fn taking_the_files_back_cancels_the_carry() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    let target = tempfile::tempdir().unwrap();
    b.plat.st.lock().unwrap().drop_dir = Some(target.path().to_path_buf());
    let (_keep, file) = make_file("secret.txt", b"nope");

    a.plat.user_drag_to_edge(Edge::Left, 0.5, vec![file]);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    until("file staged", || {
        std::fs::read_dir(b._tmp.path())
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with("carry-"))
            })
            .unwrap_or(false)
    })
    .await;
    // The mouse goes back through the edge it came in by (B's right side).
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 400,
        dy: 0,
    }));
    until("B local again", || b.node.snapshot().control == "local").await;
    until("staging cleaned", || {
        std::fs::read_dir(b._tmp.path())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("carry-"))
    })
    .await;
    assert!(std::fs::read_dir(target.path()).unwrap().next().is_none());
    assert!(b.node.snapshot().transfers.is_empty());
}

#[tokio::test]
async fn dragging_to_an_edge_with_the_option_off_does_nothing() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    a.node.set_drag_files(false);
    until("setting applied", || !a.node.snapshot().drag_files).await;
    let (_keep, file) = make_file("x.txt", b"x");
    a.plat.user_drag_to_edge(Edge::Left, 0.5, vec![file]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(b.node.snapshot().control, "local");
    assert!(!a.plat.st.lock().unwrap().capturing, "mouse handed back");
}

#[tokio::test]
async fn a_file_picked_up_on_the_second_computer_can_be_dragged_back() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    let target = tempfile::tempdir().unwrap();
    a.plat.st.lock().unwrap().drop_dir = Some(target.path().to_path_buf());
    let (_keep, file) = make_file("notes.txt", b"from the other pc");

    // A's mouse is on B; the button goes down on a file there, which B's shell reports.
    a.plat.user_push_edge(Edge::Left, 0.5);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    b.plat.st.lock().unwrap().drag_ready = Some(vec![file]);
    a.plat.emit(CaptureEvent::Input(InputEvent::Button {
        button: Button::Left,
        down: true,
    }));
    // Drag out through B's right edge, back towards A.
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 5000,
        dy: 0,
    }));
    until("A local again", || a.node.snapshot().control == "local").await;
    assert_eq!(
        b.plat.st.lock().unwrap().drag_cancels,
        1,
        "the drag on B is ended"
    );
    until("file arrived at A", || {
        std::fs::read_dir(a._tmp.path())
            .map(|d| {
                d.flatten().any(|e| {
                    e.file_name().to_string_lossy().starts_with("carry-")
                        && std::fs::read_dir(e.path())
                            .map(|mut x| x.next().is_some())
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
    .await;
    assert!(a.node.snapshot().carry.is_some(), "shown under the cursor");

    a.plat.emit(CaptureEvent::LocalLeftUp);
    until("file dropped on A", || {
        target.path().join("notes.txt").exists()
    })
    .await;
    assert_eq!(
        std::fs::read(target.path().join("notes.txt")).unwrap(),
        b"from the other pc"
    );
    until("carry over", || a.node.snapshot().carry.is_none()).await;
}

#[tokio::test]
async fn bringing_dragged_files_back_keeps_them_in_hand() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    let target = tempfile::tempdir().unwrap();
    a.plat.st.lock().unwrap().drop_dir = Some(target.path().to_path_buf());
    let (_keep, file) = make_file("keep.txt", b"still mine");
    let original = file.clone();

    a.plat.user_drag_to_edge(Edge::Left, 0.5, vec![file]);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    until("B shows the files", || b.node.snapshot().carry.is_some()).await;

    // Still holding the button, the cursor comes back out of B.
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 5000,
        dy: 0,
    }));
    until("A local again", || a.node.snapshot().control == "local").await;
    until("A shows the files", || a.node.snapshot().carry.is_some()).await;
    until("B forgot them", || b.node.snapshot().carry.is_none()).await;

    a.plat.emit(CaptureEvent::LocalLeftUp);
    until("moved on A", || target.path().join("keep.txt").exists()).await;
    assert!(!original.exists(), "moved, not copied");
    until("A done", || a.node.snapshot().carry.is_none()).await;
}

#[tokio::test]
async fn bringing_files_back_stops_the_upload() {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    pair(&a, &b).await;
    // Big enough that it is still going when the cursor turns round.
    let (_keep, file) = make_file("big.bin", &vec![1u8; 40_000_000]);

    a.plat.user_drag_to_edge(Edge::Left, 0.5, vec![file]);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    until("A shows the upload", || {
        a.node
            .snapshot()
            .transfers
            .iter()
            .any(|t| t.direction == "out")
    })
    .await;
    // Back out of B without letting go.
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 5000,
        dy: 0,
    }));
    until("A local again", || a.node.snapshot().control == "local").await;
    until("upload row gone on A", || {
        a.node
            .snapshot()
            .transfers
            .iter()
            .all(|t| t.direction != "out")
    })
    .await;
    until("download row gone on B", || {
        b.node
            .snapshot()
            .transfers
            .iter()
            .all(|t| t.direction != "in")
    })
    .await;
    until("B staging cleaned", || {
        std::fs::read_dir(b._tmp.path())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("carry-"))
    })
    .await;
    // And nothing reappears as a failure.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(a.node.snapshot().transfers.is_empty());
    assert!(b.node.snapshot().transfers.is_empty());
}

fn app(exe: &str, url: Option<&str>) -> AppRef {
    AppRef {
        name: exe.trim_end_matches(".exe").to_string(),
        exe: exe.to_string(),
        url: url.map(Into::into),
    }
}

/// B's right edge leads to A. Both have the option on unless a test says otherwise.
async fn pair_with_apps(a_on: bool, b_on: bool) -> (Side, Side) {
    let a = start("PC-A").await;
    let b = start("PC-B").await;
    a.node.set_move_apps(a_on);
    b.node.set_move_apps(b_on);
    until("options reach the platforms", || {
        a.plat.st.lock().unwrap().move_apps == a_on && b.plat.st.lock().unwrap().move_apps == b_on
    })
    .await;
    pair(&a, &b).await;
    (a, b)
}

/// The person at B has `window` in front and drags it against the edge that leads to A.
fn hand_off(b: &Side, window: AppRef) {
    b.plat.user_drag_app_to_edge(Edge::Right, window);
}

fn notice_has(s: &Side, text: &'static str) -> bool {
    s.node
        .snapshot()
        .notice
        .map(|n| n.contains(text))
        .unwrap_or(false)
}

#[tokio::test]
async fn handing_discord_over_opens_it_there_and_closes_it_here() {
    let (a, b) = pair_with_apps(true, true).await;
    hand_off(&b, app("Discord.exe", None));
    until("A opened it", || {
        !a.plat.st.lock().unwrap().launched.is_empty()
    })
    .await;
    // The person was asked where they are (B), not where the app opens.
    assert_eq!(b.plat.st.lock().unwrap().confirm_asked.len(), 1);
    assert_eq!(b.plat.st.lock().unwrap().confirm_asked[0].0, "PC-A");
    assert!(a.plat.st.lock().unwrap().confirm_asked.is_empty());
    until("B closed its own copy", || {
        !b.plat.st.lock().unwrap().closed.is_empty()
    })
    .await;
    assert_eq!(b.plat.st.lock().unwrap().closed[0].exe, "Discord.exe");
    // The cursor stays where it is: this is not a crossing.
    assert_eq!(a.node.snapshot().control, "local");
    assert_eq!(b.node.snapshot().control, "local");
}

#[tokio::test]
async fn any_program_is_closed_here_once_it_opened_there() {
    let (a, b) = pair_with_apps(true, true).await;
    hand_off(&b, app("notepad.exe", None));
    until("A opened it", || {
        !a.plat.st.lock().unwrap().launched.is_empty()
    })
    .await;
    until("closed on B", || {
        !b.plat.st.lock().unwrap().closed.is_empty()
    })
    .await;
    until("B said so", || notice_has(&b, "Moved notepad")).await;
}

#[tokio::test]
async fn a_window_on_the_controlled_computer_can_be_dragged_back() {
    let (a, b) = pair_with_apps(true, true).await;
    b.plat.st.lock().unwrap().confirm_move = true;
    // A's mouse is on B (A's left edge leads to B).
    a.plat.user_push_edge(Edge::Left, 0.5);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    // The button goes down on a window's title bar there and drags it back towards A.
    b.plat.st.lock().unwrap().app_hold = Some(app("Discord.exe", None));
    a.plat.emit(CaptureEvent::Input(InputEvent::Button {
        button: Button::Left,
        down: true,
    }));
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 5000,
        dy: 0,
    }));
    until("A opened it", || {
        !a.plat.st.lock().unwrap().launched.is_empty()
    })
    .await;
    // The question was asked where the cursor was (on B), and the mouse stayed there.
    assert_eq!(b.plat.st.lock().unwrap().confirm_asked[0].0, "PC-A");
    assert_eq!(b.node.snapshot().control, "controlled");
    assert!(
        b.plat.st.lock().unwrap().drag_cancels >= 1,
        "the window drag on B ended"
    );
    until("closed on B", || {
        !b.plat.st.lock().unwrap().closed.is_empty()
    })
    .await;
}

#[tokio::test]
async fn a_browser_window_sends_its_page_and_is_left_alone() {
    let (a, b) = pair_with_apps(true, true).await;
    let page = "https://www.youtube.com/watch?v=abc&t=42";
    hand_off(&b, app("chrome.exe", Some(page)));
    until("A opened the page", || {
        !a.plat.st.lock().unwrap().launched.is_empty()
    })
    .await;
    assert_eq!(
        a.plat.st.lock().unwrap().launched[0].url.as_deref(),
        Some(page)
    );
    until("B reported it", || notice_has(&b, "Opened the page")).await;
    assert!(
        b.plat.st.lock().unwrap().closed.is_empty(),
        "never closes a browser"
    );
}

#[tokio::test]
async fn saying_no_keeps_the_app_where_it_is_and_nothing_is_sent() {
    let (a, b) = pair_with_apps(true, true).await;
    b.plat.st.lock().unwrap().confirm_move = false;
    hand_off(&b, app("Discord.exe", None));
    until("B was asked", || {
        !b.plat.st.lock().unwrap().confirm_asked.is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.plat.st.lock().unwrap().launched.is_empty());
    assert!(b.plat.st.lock().unwrap().closed.is_empty());
}

#[tokio::test]
async fn an_app_that_cannot_be_found_there_is_not_closed_here() {
    let (a, b) = pair_with_apps(true, true).await;
    a.plat.st.lock().unwrap().launch_error =
        Some("Discord isn't installed on that computer".into());
    hand_off(&b, app("Discord.exe", None));
    until("B told why", || notice_has(&b, "isn't installed")).await;
    assert!(b.plat.st.lock().unwrap().closed.is_empty());
}

#[tokio::test]
async fn the_option_must_be_on_at_both_ends_and_dangerous_names_are_refused() {
    // Off on the receiving side: nothing opens, nothing closes, and the sender is told.
    let (a, b) = pair_with_apps(false, true).await;
    hand_off(&b, app("Discord.exe", None));
    until("B told it is off there", || notice_has(&b, "turned off")).await;
    assert!(a.plat.st.lock().unwrap().launched.is_empty());
    assert!(b.plat.st.lock().unwrap().closed.is_empty());

    // Off on the sending side: dragging does nothing at all.
    let (a2, b2) = pair_with_apps(true, false).await;
    hand_off(&b2, app("Discord.exe", None));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(b2.plat.st.lock().unwrap().confirm_asked.is_empty());
    assert!(a2.plat.st.lock().unwrap().launched.is_empty());

    // A shell is never moved, even when both ends are willing.
    let (a3, b3) = pair_with_apps(true, true).await;
    hand_off(&b3, app("cmd.exe", None));
    until("B3 is told", || notice_has(&b3, "can't be moved")).await;
    assert!(b3.plat.st.lock().unwrap().confirm_asked.is_empty());
    assert!(a3.plat.st.lock().unwrap().launched.is_empty());
}

#[tokio::test]
async fn a_side_with_no_computer_only_shows_a_message() {
    let (a, b) = pair_with_apps(true, true).await;
    // Nothing is set on B's left.
    b.plat
        .user_drag_app_to_edge(Edge::Left, app("Discord.exe", None));
    until("B told", || notice_has(&b, "No computer is set")).await;
    assert!(a.plat.st.lock().unwrap().launched.is_empty());
}

// ---------------------------------------------------------------- licences

const TEST_SIGNING_KEY: [u8; 32] = [42u8; 32];

fn pass_for(dev: &str, secs: u64) -> String {
    use tandem_core::license::*;
    let now = now();
    sign(
        &Pass {
            lic: "test".into(),
            kind: "paid".into(),
            dev: dev.into(),
            hw: "hw".into(),
            iat: now,
            exp: now + secs,
            peers: 10,
        },
        &TEST_SIGNING_KEY,
    )
}

async fn licensed_pair() -> (Side, Side) {
    let key = Some(tandem_core::license::public_of(&TEST_SIGNING_KEY));
    let a = start_with("PC-A", key).await;
    let b = start_with("PC-B", key).await;
    (a, b)
}

#[tokio::test]
async fn two_computers_without_a_licence_are_disconnected() {
    let (a, b) = licensed_pair().await;
    assert!(!a.node.snapshot().licensed);
    let code = a.node.snapshot().pairing_code;
    b.node.connect(lo(), a.node.tcp_port(), code);
    until("told why", || {
        a.node
            .snapshot()
            .error
            .unwrap_or_default()
            .contains("licence")
            && b.node
                .snapshot()
                .error
                .unwrap_or_default()
                .contains("licence")
    })
    .await;
    assert!(a.node.snapshot().connected.is_none());
    assert!(b.node.snapshot().connected.is_none());
}

#[tokio::test]
async fn one_licensed_main_is_enough_for_both() {
    let (a, b) = licensed_pair().await;
    let id = a.node.snapshot().device_id;
    a.node.set_license_pass(Some(pass_for(&id, 3600)));
    until("A licensed", || a.node.snapshot().licensed).await;
    pair(&a, &b).await;
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(a.node.snapshot().connected.is_some());
    assert!(
        b.node.snapshot().connected.is_some(),
        "B (no licence) stays connected"
    );
    assert!(!b.node.snapshot().licensed);
}

#[tokio::test]
async fn a_pass_copied_from_another_computer_does_not_count() {
    let (a, b) = licensed_pair().await;
    // A shows a pass that was made for B.
    let other = b.node.snapshot().device_id;
    a.node.set_license_pass(Some(pass_for(&other, 3600)));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!a.node.snapshot().licensed);
    let code = a.node.snapshot().pairing_code;
    b.node.connect(lo(), a.node.tcp_port(), code);
    until("refused", || {
        b.node
            .snapshot()
            .error
            .unwrap_or_default()
            .contains("licence")
    })
    .await;
    assert!(b.node.snapshot().connected.is_none());
}

#[tokio::test]
async fn losing_the_licence_disconnects_the_free_computers() {
    let (a, b) = licensed_pair().await;
    let id = a.node.snapshot().device_id;
    a.node.set_license_pass(Some(pass_for(&id, 3600)));
    pair(&a, &b).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The licence moved to another computer: the app takes the pass away.
    a.node.set_license_pass(None);
    until("dropped", || b.node.snapshot().connected.is_none()).await;
}

/// The person at B drags a window through the edge that leads to A, like onto a second
/// monitor (what the Windows layer reports), and lets go `crossed` px over, of 1200.
fn drag_across(b: &Side, window: AppRef, crossed: i32) {
    b.plat.emit(CaptureEvent::SpanBegin {
        edge: Edge::Right,
        w: 1200,
        h: 800,
    });
    for c in [100, crossed / 2, crossed] {
        b.plat.emit(CaptureEvent::SpanMove {
            crossed: c,
            along: 0.25,
            gx: 300,
            gy: 15,
        });
    }
    b.plat.emit(CaptureEvent::SpanFrame(vec![0xFF, 0xD8, 0xFF]));
    let moved = crossed * 2 > 1200;
    b.plat.emit(CaptureEvent::SpanEnd {
        moved,
        app: Some(window),
        frac: 0.3,
    });
    if moved {
        b.plat.emit(CaptureEvent::Crossed {
            edge: Edge::Right,
            frac: 0.3,
        });
    }
}

#[tokio::test]
async fn a_window_dragged_across_shows_there_and_moves_when_let_go() {
    let (a, b) = pair_with_apps(true, true).await;
    drag_across(&b, app("notepad.exe", None), 700);
    until("A shows the window coming in", || {
        a.plat.st.lock().unwrap().span_frames == 1
    })
    .await;
    {
        let st = a.plat.st.lock().unwrap();
        let last = st.span_shown.last().copied().expect("shown");
        // B's right edge is A's left: the window comes in from A's left.
        assert_eq!(last.entry, Edge::Left);
        assert_eq!(
            (last.crossed, last.w, last.h, last.gx),
            (700, 1200, 800, 300)
        );
        assert!((last.along - 0.25).abs() < 1e-6);
    }
    until("A opened it", || {
        !a.plat.st.lock().unwrap().launched.is_empty()
    })
    .await;
    // Let go over there on purpose: nobody was asked.
    assert!(b.plat.st.lock().unwrap().confirm_asked.is_empty());
    until("the picture goes once it opened", || {
        a.plat.st.lock().unwrap().span_hidden.contains(&900)
    })
    .await;
    until("closed on B", || {
        !b.plat.st.lock().unwrap().closed.is_empty()
    })
    .await;
    // And the mouse carried on to A.
    until("B drives A", || b.node.snapshot().control == "remote").await;
}

#[tokio::test]
async fn letting_go_before_halfway_keeps_the_window_here() {
    let (a, b) = pair_with_apps(true, true).await;
    drag_across(&b, app("notepad.exe", None), 400);
    until("the picture on A goes away at once", || {
        a.plat.st.lock().unwrap().span_hidden.contains(&0)
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(a.plat.st.lock().unwrap().launched.is_empty());
    assert_eq!(b.node.snapshot().control, "local");
}

#[tokio::test]
async fn a_window_that_cannot_open_there_comes_back() {
    let (a, b) = pair_with_apps(true, true).await;
    a.plat.st.lock().unwrap().launch_error = Some("Notepad isn't installed".into());
    drag_across(&b, app("notepad.exe", None), 900);
    until("B put its window back", || {
        b.plat.st.lock().unwrap().span_restores >= 1
    })
    .await;
    assert!(b.plat.st.lock().unwrap().closed.is_empty());
    until("B says why", || notice_has(&b, "isn't installed")).await;
}

#[tokio::test]
async fn a_window_whose_program_cannot_move_comes_back_with_a_note() {
    let (_a, b) = pair_with_apps(true, true).await;
    b.plat.emit(CaptureEvent::SpanBegin {
        edge: Edge::Right,
        w: 800,
        h: 600,
    });
    b.plat.emit(CaptureEvent::SpanEnd {
        moved: true,
        app: None,
        frac: 0.5,
    });
    until("B put it back", || {
        b.plat.st.lock().unwrap().span_restores >= 1
    })
    .await;
    until("B says so", || notice_has(&b, "can't be moved")).await;
}

#[tokio::test]
async fn a_window_on_the_controlled_computer_hangs_across_on_the_way_back() {
    let (a, b) = pair_with_apps(true, true).await;
    // A's mouse is on B (A's left edge leads to B, so B's way back is its right edge).
    a.plat.user_push_edge(Edge::Left, 0.5);
    until("B controlled", || b.node.snapshot().control == "controlled").await;
    // There, a window is grabbed by its title bar and dragged back towards A.
    b.plat.st.lock().unwrap().span_window = Some(app("notepad.exe", None));
    a.plat.emit(CaptureEvent::Input(InputEvent::Button {
        button: Button::Left,
        down: true,
    }));
    a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
        dx: 5000,
        dy: 0,
    }));
    until("B hangs it across its right edge", || {
        b.plat.st.lock().unwrap().span_taken == Some(Edge::Right)
    })
    .await;
    // The mouse keeps going: B moves the window instead of the cursor, and A shows it.
    for _ in 0..4 {
        a.plat.emit(CaptureEvent::Input(InputEvent::MouseMove {
            dx: 200,
            dy: 0,
        }));
    }
    until("A shows it coming in", || {
        a.plat
            .st
            .lock()
            .unwrap()
            .span_shown
            .last()
            .is_some_and(|v| v.crossed >= 800)
    })
    .await;
    assert_eq!(
        a.plat.st.lock().unwrap().span_shown.last().unwrap().entry,
        Edge::Left
    );
    assert_eq!(
        b.node.snapshot().control,
        "controlled",
        "still on B while dragging"
    );
    // Let go over A: it opens on A, closes on B, and the mouse is back on A.
    a.plat.emit(CaptureEvent::Input(InputEvent::Button {
        button: Button::Left,
        down: false,
    }));
    until("A opened it", || {
        !a.plat.st.lock().unwrap().launched.is_empty()
    })
    .await;
    until("closed on B", || {
        !b.plat.st.lock().unwrap().closed.is_empty()
    })
    .await;
    until("the mouse is back on A", || {
        a.node.snapshot().control == "local"
    })
    .await;
    assert!(
        b.plat.st.lock().unwrap().confirm_asked.is_empty(),
        "nobody was asked"
    );
}
