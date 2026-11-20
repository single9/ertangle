use super::*;
use ertangle_core::{Cost, Role, Session};
use ratatui::backend::TestBackend;
use std::collections::VecDeque;

fn sample_app(lines: &[&str]) -> App {
    let mut app = test_app();
    let s = Session {
        id: "ses_test".into(),
        kind: AgentKind::OpenCode,
        label: "worker".into(),
        cwd: "/tmp".into(),
        branch: Some("ertangle/x".into()),
        role: Role::Worker,
        status: SessionStatus::Busy,
        model: None,
        cost: Cost::default(),
        parent: None,
        created_ms: 1,
        transport: "acp".into(),
        status_note: None,
        repo_root: None,
        context: None,
        native_id: None,
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };
    app.state.sessions.insert(s.id.clone(), s);
    let deque: VecDeque<String> = lines.iter().map(|l| l.to_string()).collect();
    app.state.transcripts.insert("ses_test".into(), deque);
    app
}

fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect()
}

fn render(app: &mut App) -> String {
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    buffer_text(&term)
}

#[test]
fn transcript_renders_markdown_and_toggles_raw() {
    let mut app = sample_app(&[
        "# Title",
        "a **bold** word",
        "```rust",
        "let x = 1;",
        "```",
        "| a | b |",
        "|---|---|",
        "| 1 | 2 |",
    ]);

    let rendered = render(&mut app);
    assert!(rendered.contains("Title"), "heading text missing");
    assert!(rendered.contains("bold"), "bold text missing");
    assert!(!rendered.contains("**bold**"), "markdown not stripped");
    assert!(rendered.contains("let x = 1;"), "code block missing");
    assert!(!rendered.contains("[raw]"), "md 是預設，不該標示 raw");

    app.raw_md = true;
    let raw = render(&mut app);
    assert!(raw.contains("**bold**"), "raw mode should show markup");
    assert!(raw.contains("[raw]"), "偏離預設時要在標題列標示");
}

#[test]
fn transcript_footer_shows_model_and_effort() {
    let mut app = sample_app(&["hi"]);
    {
        let s = app.state.sessions.get_mut("ses_test").unwrap();
        s.model = Some("opus-4.5".into());
        s.effort = Some("high".into());
        s.permission = PermissionPolicy::Bypass;
    }
    let text = render(&mut app);
    assert!(text.contains("opencode:opus-4.5"), "model missing: {text}");
    assert!(text.contains("effort high"), "effort missing");
    assert!(text.contains("perm bypass"), "permission missing");
}

#[test]
fn diff_view_shows_gutter_and_stats() {
    let mut app = sample_app(&[]);
    app.view = CenterView::Diff;
    app.diff_lines = vec![
        "diff --git a/f b/f".into(),
        "--- a/f".into(),
        "+++ b/f".into(),
        "@@ -1,2 +1,3 @@".into(),
        " ctx".into(),
        "-old".into(),
        "+new".into(),
        "+extra".into(),
    ];
    app.diff_gutter = build_gutter(&app.diff_lines);
    let (added, removed) = diff_stat(&app.diff_lines);
    app.diff_added = added;
    app.diff_removed = removed;
    assert_eq!((added, removed), (2, 1));

    let text = render(&mut app);
    assert!(text.contains("+2 -1"), "diff stat missing");
    assert!(text.contains("new"), "added line missing");
}

#[test]
fn scroll_transcript_pages_and_resumes_follow() {
    let mut app = sample_app(&[]);
    app.transcript_total = 100;
    app.transcript_viewport = 10; // 跟隨時 offset = 90

    scroll_transcript(&mut app, -1);
    assert_eq!(app.transcript_scroll, Some(80));

    scroll_transcript(&mut app, 1);
    assert_eq!(app.transcript_scroll, None, "到底應恢復跟隨");

    app.transcript_scroll = Some(0);
    scroll_transcript(&mut app, -1);
    assert_eq!(app.transcript_scroll, Some(0), "不應低於 0");
}

#[test]
fn scroll_transcript_lines_moves_by_rows() {
    let mut app = sample_app(&[]);
    app.transcript_total = 100;
    app.transcript_viewport = 10;

    scroll_transcript_lines(&mut app, -3);
    assert_eq!(app.transcript_scroll, Some(87));
    scroll_transcript_lines(&mut app, 3);
    assert_eq!(app.transcript_scroll, None, "回到最新應恢復跟隨");
    scroll_transcript_lines(&mut app, -1000);
    assert_eq!(app.transcript_scroll, Some(0), "不應低於 0");
}

#[test]
fn user_prompt_is_echoed_in_transcript() {
    let mut app = sample_app(&[]);
    app.state.apply(&ertangle_core::Record {
        seq: 1,
        ts: 1,
        event: ertangle_core::Event::UserPrompt {
            session: "ses_test".into(),
            text: "fix the tests\nsecond line 中文".into(),
            from: None,
        },
    });

    // CJK 是雙寬字元，TestBackend 會在中間留下空白格，因此用 ASCII 檢查版面。
    let text = render(&mut app);
    assert!(text.contains("❯ fix the tests"), "prompt marker missing");
    assert!(text.contains("second line"), "continuation line missing");

    let buf = &app.state.transcripts["ses_test"];
    assert_eq!(buf[0], "❯ fix the tests");
    assert_eq!(buf[1], "  second line 中文");
}

#[test]
fn base64_matches_known_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode("註".as_bytes()), "6Ki7");
}

#[test]
fn base64_utf8_roundtrip() {
    use crate::clipboard::base64_decode;
    let samples = [
        "",
        "f",
        "fo",
        "foo",
        "註",
        "Hello, World!",
        "繁體中文 台灣軟體工程用語 (Terminal-native)",
        "Multiline\nText\tWith\r\nSpecial\0Chars 🚀",
    ];
    for s in samples {
        let enc = base64_encode(s.as_bytes());
        let dec = base64_decode(&enc).expect("decode failed");
        let roundtrip = String::from_utf8(dec).expect("valid utf-8");
        assert_eq!(roundtrip, s);
    }
}

#[test]
fn choose_native_clipboard_backend() {
    use crate::clipboard::{choose_native, ClipBackend};

    // 1. 遠端 SSH 環境：一律為 None，偏好使用 OSC 52
    assert_eq!(choose_native(true, true, |_| true), None);
    assert_eq!(choose_native(true, false, |_| true), None);

    // 2. 本機 Wayland 環境：優先選擇 wl-copy
    let have_all = |_bin: &str| true;
    assert_eq!(
        choose_native(false, true, have_all),
        Some(ClipBackend::WlCopy)
    );

    // 本機 Wayland 但無 wl-copy：回退至 xclip
    let no_wl = |bin: &str| bin != "wl-copy";
    assert_eq!(choose_native(false, true, no_wl), Some(ClipBackend::Xclip));

    // 本機 Wayland 無 wl-copy 且無 xclip：回退至 xsel
    let xsel_only = |bin: &str| bin == "xsel";
    assert_eq!(
        choose_native(false, true, xsel_only),
        Some(ClipBackend::Xsel)
    );

    // 本機 Wayland 但無任何工具：None
    let have_none = |_bin: &str| false;
    assert_eq!(choose_native(false, true, have_none), None);

    // 3. 本機 X11 環境 (wayland = false)：忽略 wl-copy，優先選擇 xclip
    assert_eq!(
        choose_native(false, false, have_all),
        Some(ClipBackend::Xclip)
    );
    assert_eq!(
        choose_native(false, false, xsel_only),
        Some(ClipBackend::Xsel)
    );
    assert_eq!(choose_native(false, false, have_none), None);
}

#[test]
fn osc52_plain_and_tmux_wrapped_bytes() {
    use crate::clipboard::osc52_sequence;

    // Plain: \x1b]52;c;<base64>\x07
    let plain = osc52_sequence("hello", false);
    assert_eq!(plain, b"\x1b]52;c;aGVsbG8=\x07");

    // Inside tmux: \x1bPtmux;\x1b\x1b]52;c;<base64>\x07\x1b\\
    let tmux = osc52_sequence("hello", true);
    assert_eq!(tmux, b"\x1bPtmux;\x1b\x1b]52;c;aGVsbG8=\x07\x1b\\");
}

#[test]
fn main_pane_text_excludes_sidebars() {
    let mut app = sample_app(&["hello", "world"]);
    assert_eq!(main_pane_text(&app), "hello\nworld");

    app.view = CenterView::Diff;
    app.diff_lines = vec!["+added".into(), "-removed".into()];
    assert_eq!(main_pane_text(&app), "+added\n-removed");
}

#[test]
fn input_editing_respects_unicode_cursor() {
    let mut app = test_app();
    app.mode = Mode::Prompt;
    insert_str_input(&mut app, "你好");
    assert_eq!(app.input, "你好");
    assert_eq!(app.input_cursor, 2);

    app.input_cursor = 1;
    insert_input(&mut app, 'X');
    assert_eq!(app.input, "你X好");
    assert_eq!(app.input_cursor, 2);

    backspace_input(&mut app);
    assert_eq!(app.input, "你好");
    assert_eq!(app.input_cursor, 1);

    delete_input(&mut app);
    assert_eq!(app.input, "你");

    app.input_cursor = 0;
    backspace_input(&mut app);
    assert_eq!(app.input, "你", "最前面按退格不應改動");
}

#[test]
fn input_cursor_is_placed_inside_the_box() {
    let mut app = sample_app(&[]);
    app.mode = Mode::Prompt;
    app.input = "你好".into();
    app.input_cursor = 2;

    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let pos = term.get_cursor_position().unwrap();
    // 輸入框在倒數第二列；內框起點為 (1, 37)，游標應落在「你好」之後。
    assert_eq!((pos.x, pos.y), (5, 37));
}

#[test]
fn thinking_lines_are_hidden_by_default() {
    let mut app = sample_app(&["💭 planning", "answer", "💭 more", "done"]);
    let id: SessionId = "ses_test".into();

    let (lines, _) = build_transcript_lines(&app, &id, 80);
    let text: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("planning"), "thought leaked: {text}");
    assert!(!text.contains("more"), "thought leaked: {text}");
    assert!(text.contains("answer"));
    assert!(text.contains("done"));
    assert!(!main_pane_text(&app).contains("planning"));

    app.show_thoughts = true;
    let (lines, _) = build_transcript_lines(&app, &id, 80);
    let text: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("planning"));
    assert!(text.contains("more"));
    assert!(main_pane_text(&app).contains("planning"));
}

fn cache_lines(lines: Vec<Line<'static>>) -> MdCache {
    MdCache {
        session: "ses_test".into(),
        stamp: TranscriptStamp::default(),
        raw: false,
        width: 80,
        show_tools: false,
        show_thoughts: false,
        lines,
        links: Vec::new(),
        prefix_raw_len: 0,
        prefix_lines_count: 0,
        prefix_links_count: 0,
        prefix_front_hash: 0,
        prefix_front_len: 0,
        prefix_last_hash: 0,
        prefix_last_len: 0,
    }
}

#[test]
fn selection_text_uses_display_columns() {
    let mut app = sample_app(&[]);
    app.md_cache = Some(cache_lines(vec![
        Line::raw("hello 你好 world"),
        Line::raw("second"),
    ]));

    // 「你好」從顯示欄 6 開始，各佔 2 欄。
    app.selection = Some(((0, 6), (0, 9)));
    assert_eq!(selection_text(&app), "你好");

    // 跨行：第一行尾端到第二行開頭。
    app.selection = Some(((0, 11), (1, 1)));
    assert_eq!(selection_text(&app), "world\nse");

    // 反向拖曳 (由下往上)也要正常。
    app.selection = Some(((1, 1), (0, 11)));
    assert_eq!(selection_text(&app), "world\nse");
}

#[test]
fn highlight_columns_marks_only_selected_range() {
    let out = highlight_columns(&Line::raw("abcd"), 1, 3);
    let spans: Vec<(String, bool)> = out
        .spans
        .iter()
        .map(|s| {
            (
                s.content.to_string(),
                s.style.add_modifier.contains(Modifier::REVERSED),
            )
        })
        .collect();
    assert_eq!(
        spans,
        vec![
            ("a".to_string(), false),
            ("bc".to_string(), true),
            ("d".to_string(), false),
        ]
    );
}

#[test]
fn selection_columns_cover_full_middle_lines() {
    let sel = ((2, 5), (4, 1));
    assert_eq!(selection_columns(sel, 1, 40), None);
    assert_eq!(selection_columns(sel, 2, 40), Some((5, 40)));
    assert_eq!(selection_columns(sel, 3, 40), Some((0, 40)));
    assert_eq!(selection_columns(sel, 4, 40), Some((0, 2)));
    assert_eq!(selection_columns(sel, 5, 40), None);
}

#[test]
fn mouse_selection_copies_only_middle_pane() {
    let mut app = sample_app(&["alpha", "beta", "gamma"]);
    let _ = render(&mut app); // 先畫一次，建立 md_cache 與 transcript_area
    app.selection = Some(((1, 0), (1, 3)));
    let text = selection_text(&app);
    assert_eq!(text, "beta");
    assert!(!text.contains("Sessions"), "side pane leaked: {text}");

    let rendered = render(&mut app);
    assert!(rendered.contains("beta"));
}

#[test]
fn clean_candidates_include_ended_sessions_with_worktrees() {
    let mut app = sample_app(&[]);
    // 測試只看 app.state，不連任何遠端主機。
    app.hosts.clear();
    app.state.sessions.get_mut("ses_test").unwrap().status = SessionStatus::Exited;

    add_session(&mut app, "ses_ended", "ended", 10);
    {
        let s = app.state.sessions.get_mut("ses_ended").unwrap();
        s.status = SessionStatus::Exited;
        s.branch = None;
    }
    add_session(&mut app, "ses_branch", "branch", 11);
    app.state.sessions.get_mut("ses_branch").unwrap().status = SessionStatus::Exited;

    let ids = clean_candidates(&app);
    assert_eq!(
        ids.len(),
        3,
        "ended sessions (含 worktree) 都要列入：{ids:?}"
    );
    assert!(ids.contains(&"ses_ended".to_string()));
    assert!(ids.contains(&"ses_branch".to_string()));

    app.state.sessions.get_mut("ses_ended").unwrap().status = SessionStatus::Busy;
    let ids = clean_candidates(&app);
    assert_eq!(ids.len(), 2);
    assert!(!ids.contains(&"ses_ended".to_string()), "執行中的不能列入");
}

#[test]
fn transcript_pos_maps_rows_and_columns() {
    let mut app = sample_app(&[]);
    app.transcript_area = Rect::new(0, 0, 40, 10);
    app.transcript_inner_w = 37;
    app.transcript_total = 100;
    app.transcript_offset = 5;

    // 內框起點是 (1, 1)，所以 (3, 2) → 行 6、欄 2。
    assert_eq!(transcript_pos(&app, 3, 2, false), Some((6, 2)));
    // 區域外不啟動選取。
    assert_eq!(transcript_pos(&app, 39, 2, false), None);
    // 拖曳出界時夾在可見範圍。
    assert_eq!(transcript_pos(&app, 3, 99, true), Some((12, 2)));
    assert_eq!(transcript_pos(&app, 0, 2, true), Some((6, 0)));
}

// ---- T13：版面模式 ----

fn add_session(app: &mut App, id: &str, label: &str, created: u64) {
    let mut s = app.state.sessions["ses_test"].clone();
    s.id = id.into();
    s.label = label.into();
    s.created_ms = created;
    app.state.sessions.insert(s.id.clone(), s);
}

fn render_size(app: &mut App, w: u16, h: u16) -> (Terminal<TestBackend>, String) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let text = buffer_text(&term);
    (term, text)
}

fn row_text(term: &Terminal<TestBackend>, y: u16) -> String {
    let buf = term.backend().buffer();
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

/// 寬字元後面會跟一個空白填充格，比對時先去掉全部空白。
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn pair_writer() -> (HostRouter, std::os::unix::net::UnixStream) {
    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
    b.set_nonblocking(true).unwrap();
    (HostRouter::single(ertangle_daemon::ConnStream::Unix(a)), b)
}

fn add_pending(app: &mut App) {
    app.state.pending_permissions.insert(
        "r1".into(),
        ertangle_core::PermissionRequest {
            req: "r1".into(),
            session: "ses_test".into(),
            title: "Bash".into(),
            detail: "rm -rf x".into(),
        },
    );
}

#[test]
fn layout_modes_render_expected_panes() {
    let mut app = sample_app(&["hi"]);
    assert_eq!(app.layout, LayoutMode::Two, "預設是兩欄 (不顯示 Bus)");
    app.layout = LayoutMode::Three;
    let (_, three) = render_size(&mut app, 120, 40);
    assert!(three.contains("Sessions") && three.contains("Project"));
    assert!(!three.contains("Bus / Tasks"));
    app.right_bus = true;
    let (_, three_bus) = render_size(&mut app, 120, 40);
    assert!(three_bus.contains("Bus / Tasks"));
    app.right_bus = false;

    app.layout = LayoutMode::Two;
    let (_, two) = render_size(&mut app, 120, 40);
    assert!(two.contains("Sessions") && !two.contains("Bus / Tasks"));

    app.layout = LayoutMode::Tabs;
    let (term, tabs) = render_size(&mut app, 120, 40);
    assert!(!tabs.contains("Sessions") && !tabs.contains("Bus / Tasks"));
    assert!(row_text(&term, 2).contains("worker"));
    // 中央區塊佔滿整個寬度。
    assert_eq!(app.transcript_area.width, 120);
    assert_eq!(app.sessions_area, Rect::default());
}

#[test]
fn tabs_keep_selected_visible_when_overflowing() {
    let mut app = sample_app(&["hi"]);
    for i in 0..12 {
        add_session(
            &mut app,
            &format!("ses_{i:02}"),
            &format!("label-{i:02}"),
            10 + i,
        );
    }
    app.layout = LayoutMode::Tabs;
    let last = app.session_ids().len() - 1;
    for sel in [0, 5, last] {
        app.selected = sel;
        let (term, _) = render_size(&mut app, 50, 20);
        let label = app
            .state
            .sessions
            .get(&app.session_ids()[sel])
            .unwrap()
            .label
            .clone();
        assert!(row_text(&term, 2).contains(&label), "sel {sel} hidden");
        assert!(app.tab_areas.iter().any(|(_, i)| *i == sel));
    }
    let (term, _) = render_size(&mut app, 50, 20);
    assert!(row_text(&term, 2).contains('‹'));
}

#[test]
fn hidden_bus_shows_pending_banner_and_allows() {
    use std::io::Read;
    let mut app = sample_app(&["hi"]);
    add_pending(&mut app);
    app.layout = LayoutMode::Two;
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(squash(&text).contains("要求Bash"), "banner missing");

    let (mut w, mut peer) = pair_writer();
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    )
    .unwrap();
    let mut bytes = [0u8; 1024];
    let n = peer.read(&mut bytes).unwrap();
    let buf = String::from_utf8_lossy(&bytes[..n]).to_string();
    assert!(
        buf.contains("\"permission\"") && buf.contains("r1"),
        "got {buf}"
    );
}

#[test]
fn switching_layout_clears_stale_click_areas() {
    let mut app = sample_app(&["hi"]);
    add_session(&mut app, "ses_b", "second", 5);
    let _ = render_size(&mut app, 120, 40);
    let click = (app.sessions_area.x + 2, app.sessions_area.y + 2);
    app.layout = LayoutMode::Tabs;
    let _ = render_size(&mut app, 120, 40);
    app.selected = 0;
    let (mut w, _peer) = pair_writer();
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: click.0,
        row: click.1,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, &mut w, ev).unwrap();
    assert_eq!(app.selected, 0);

    // 點分頁可切換 session。
    let (rect, idx) = *app.tab_areas.iter().find(|(_, i)| *i == 1).unwrap();
    let ev = MouseEvent {
        column: rect.x,
        row: rect.y,
        ..ev
    };
    handle_mouse(&mut app, &mut w, ev).unwrap();
    assert_eq!(app.selected, idx);
}

#[test]
fn clicking_each_session_row_selects_that_session() {
    let mut app = sample_app(&["hi"]);
    add_session(&mut app, "ses_b", "second", 5);
    app.layout = LayoutMode::Two;
    let _ = render_size(&mut app, 120, 40);
    let x = app.sessions_area.x + 2;
    let first_row = app.sessions_area.y + 1;
    let (mut w, _peer) = pair_writer();

    // 每一筆只佔一行：第一列選第一個 session、第二列選第二個。
    for (row, want) in [(first_row, 0usize), (first_row + 1, 1usize)] {
        app.selected = 1;
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, &mut w, ev).unwrap();
        assert_eq!(app.selected, want, "row {row} 應選到第 {want} 筆");
    }
}

#[test]
fn three_degrades_on_narrow_terminal() {
    assert_eq!(effective_layout(LayoutMode::Three, 120), LayoutMode::Three);
    assert_eq!(effective_layout(LayoutMode::Three, 70), LayoutMode::Two);
    assert_eq!(effective_layout(LayoutMode::Three, 60), LayoutMode::Tabs);
    assert_eq!(effective_layout(LayoutMode::Tabs, 200), LayoutMode::Tabs);
    let mut app = sample_app(&["hi"]);
    let (_, text) = render_size(&mut app, 60, 30);
    assert!(!text.contains("Bus / Tasks"));
    assert!(app.transcript_area.width >= 40);
}

#[test]
fn layout_key_command_and_unread_counter() {
    let mut app = sample_app(&["hi"]);
    let (mut w, _peer) = pair_writer();
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
    )
    .unwrap();
    // 預設兩欄，L 依序切到分頁。
    assert_eq!(app.layout, LayoutMode::Tabs);
    execute_command(&mut app, &mut w, "/layout tabs").unwrap();
    assert_eq!(app.layout, LayoutMode::Tabs);
    execute_command(&mut app, &mut w, "/layout three").unwrap();
    assert_eq!(app.layout, LayoutMode::Three);

    app.layout = LayoutMode::Two;
    let _ = render_size(&mut app, 120, 40);
    app.state.messages.push(ertangle_core::BusMessage {
        id: "m1".into(),
        from: "a".into(),
        to: Target::Broadcast,
        kind: MsgKind::Chat,
        body: "yo".into(),
        reply_to: None,
        ts: 1,
    });
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(squash(&text).contains("1未讀"));
    app.bus_overlay = true;
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("Bus / Tasks") && !squash(&text).contains("未讀"));
}

#[test]
fn bus_messages_follow_project_scope() {
    let mut app = sample_app(&[]);
    app.home_project = "/work/alpha".into();
    app.scope_here = true;
    app.state.sessions.get_mut("ses_test").unwrap().cwd = "/work/alpha".into();
    add_session(&mut app, "ses_beta", "beta", 5);
    {
        let s = app.state.sessions.get_mut("ses_beta").unwrap();
        s.cwd = "/work/beta".into();
        s.branch = None;
    }
    let msg = |id: &str, from: &str, to: Target| BusMessage {
        id: id.into(),
        from: from.into(),
        to,
        kind: MsgKind::Chat,
        body: id.into(),
        reply_to: None,
        ts: 1,
    };
    let direct = |t: &str| Target::Direct(t.into());
    for m in [
        msg("m1", "ses_test", Target::Broadcast),
        msg("m2", "ses_beta", Target::Broadcast),
        msg("m3", "ses_beta", direct("ses_test")),
        msg("m4", "ses_test", direct("ses_beta")),
        msg("m5", "ses_beta", direct("beta")),
        msg("m6", "ses_gone", Target::Broadcast),
        msg("m7", "ses_beta", Target::Channel("review".into())),
    ] {
        app.state.messages.push(m);
    }

    let ids: Vec<&str> = app
        .visible_messages()
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["m1", "m3", "m4", "m6"],
        "本專案發送、寄給本專案、或無法判斷的訊息才顯示"
    );

    // 未讀只算畫面上看得到的訊息；前 3 則已看過。
    app.bus_visible = false;
    app.bus_seen = 3;
    assert_eq!(app.unread_bus(), 2, "m4 與 m6 尚未看過");

    app.scope_here = false;
    assert_eq!(app.visible_messages().len(), 7);
    assert_eq!(app.unread_bus(), 4);
}

#[test]
fn initial_snapshot_does_not_count_bus_history_as_unread() {
    let mut app = test_app();
    app.bus_visible = false;
    let msg = |id: u64| BusMessage {
        id: format!("m{id}"),
        from: "ses_a".into(),
        to: Target::Broadcast,
        kind: MsgKind::Chat,
        body: "x".into(),
        reply_to: None,
        ts: id,
    };
    let mut state = State::new();
    for i in 0..3 {
        state.messages.push(msg(i));
    }
    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Snapshot {
            state: Box::new(state),
        },
    );
    assert_eq!(app.unread_bus(), 0, "啟動前的歷史訊息不算未讀");

    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Record {
            record: Box::new(ertangle_core::Record {
                seq: 1,
                ts: 1,
                event: ertangle_core::Event::BusMessage { message: msg(9) },
            }),
        },
    );
    assert_eq!(app.unread_bus(), 1, "啟動後新進的才算未讀");
}

#[test]
fn bus_unread_accounts_for_message_cap_trimming() {
    let mut app = test_app();
    app.bus_visible = false;
    let msg = |id: u64| BusMessage {
        id: format!("m{id}"),
        from: "ses_a".into(),
        to: Target::Broadcast,
        kind: MsgKind::Chat,
        body: "x".into(),
        reply_to: None,
        ts: id,
    };
    let mut state = State::new();
    for i in 0..ertangle_core::MESSAGE_CAP {
        state.messages.push(msg(i as u64));
    }
    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Snapshot {
            state: Box::new(state),
        },
    );
    assert_eq!(app.unread_bus(), 0);

    // 滿額後新增一則會從前端丟棄一則；已看過的索引要跟著前移。
    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Record {
            record: Box::new(ertangle_core::Record {
                seq: 1,
                ts: 1,
                event: ertangle_core::Event::BusMessage {
                    message: msg(ertangle_core::MESSAGE_CAP as u64),
                },
            }),
        },
    );
    assert_eq!(app.state.messages.len(), ertangle_core::MESSAGE_CAP);
    assert_eq!(app.unread_bus(), 1);
}

#[test]
fn header_shows_log_failure_indicator() {
    let mut app = sample_app(&["hi"]);
    let (_, text_ok) = render_size(&mut app, 120, 40);
    assert!(!text_ok.contains("log ✗"));

    app.state.log_ok = false;
    let (_, text_err) = render_size(&mut app, 120, 40);
    assert!(text_err.contains("log ✗"));
}

#[test]
fn tool_lines_are_not_mangled_by_markdown() {
    let mut app = sample_app(&["x"]);
    let id = app.selected_id().unwrap();
    app.state
        .transcripts
        .get_mut(&id)
        .unwrap()
        .push_back("▸ mcp__ertangle__ertangle_peers: {\"timeout_ms\":1}".to_string());
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("mcp__ertangle__ertangle_peers"), "{text}");
    assert!(text.contains("timeout_ms"));
}

#[test]
fn space_menu_tree_has_session_and_git_groups() {
    let (_, root) = crate::menu::menu_items(&[]).unwrap();
    assert!(root.iter().any(|i| i.key == 's') && root.iter().any(|i| i.key == 'g'));
    let (_, s) = crate::menu::menu_items(&['s']).unwrap();
    assert!(s.iter().any(|i| i.key == 'n') && s.iter().any(|i| i.key == 'x'));
    let (_, g) = crate::menu::menu_items(&['g']).unwrap();
    assert!(g
        .iter()
        .any(|i| i.key == 'r' && matches!(i.act, crate::menu::Act::Cmd("merge --resolve"))));
    assert!(crate::menu::menu_items(&['z']).is_none());
}

#[test]
fn picker_filters_and_offers_default_model() {
    let choices = crate::menu::model_choices(&AgentKind::ClaudeCode);
    assert_eq!(choices[0].1, "");
    assert!(choices.iter().any(|(_, v)| v == "opus"));
    // 別名與固定版本都要能選。
    assert!(choices.iter().any(|(_, v)| v == "fable"));
    assert!(choices.iter().any(|(_, v)| v == "claude-opus-5-5"));
    assert!(choices.iter().any(|(_, v)| v == "claude-sonnet-4-6"));
    let p = crate::menu::Picker {
        stage: crate::menu::PickStage::Agent,
        resume_scope: Default::default(),
        title: "t".into(),
        items: choices,
        filter: "op".into(),
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
    };
    let vis = p.visible();
    assert!(vis
        .iter()
        .all(|(l, v)| l.contains("op") || v.contains("op")));
    assert!(!vis.is_empty());
}

#[test]
fn parses_models_output_and_skips_status_lines() {
    let out = "Fetching available models...\ngemini-4-argon-medium\tGemini 4 Argon (Medium)\nollama/gemma4:12b\n\n";
    let m = ertangle_adapter::models::parse_models(out);
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].1, "gemini-4-argon-medium");
    assert!(m[0].0.contains("Gemini 4 Argon"));
    assert_eq!(
        m[1],
        (
            "ollama/gemma4:12b".to_string(),
            "ollama/gemma4:12b".to_string()
        )
    );
}

#[test]
fn command_hints_list_matches_then_usage() {
    let all = crate::commands::command_hints("");
    assert!(all.len() > 10);
    let m = crate::commands::command_hints("me");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].0, "merge");
    let usage = crate::commands::command_hints("merge --dr");
    assert_eq!(usage.len(), 1);
    assert!(usage[0].1.contains("dry-run"));
    let usage_res = crate::commands::command_hints("merge --re");
    assert_eq!(usage_res.len(), 1);
    assert!(usage_res[0].1.contains("resolve"));
    assert!(crate::commands::command_hints("zzz").is_empty());
}

#[test]
fn parses_codex_model_catalog_visible_only() {
    let out = r#"{"models":[{"slug":"a","display_name":"A","visibility":"list"},{"slug":"b","display_name":"B","visibility":"hide"}]}"#;
    let m = ertangle_adapter::models::parse_codex_models(out);
    assert_eq!(m, vec![("a  (A)".to_string(), "a".to_string())]);
    assert!(ertangle_adapter::models::parse_codex_models("not json").is_empty());
}

#[test]
fn effort_suffix_is_split_from_model_id() {
    use crate::menu::split_effort_suffix;
    assert_eq!(
        split_effort_suffix("gemini-4-argon-medium"),
        Some(("gemini-4-argon", "medium"))
    );
    assert_eq!(
        split_effort_suffix("gemini-4-hydrogen-xhigh"),
        Some(("gemini-4-hydrogen", "xhigh"))
    );
    assert_eq!(split_effort_suffix("gemini-pro-orchestrator-g1-pro"), None);
    assert_eq!(split_effort_suffix("ollama/gemma4:12b"), None);
}

#[test]
fn antigravity_models_are_grouped_by_base_name() {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(vec![
        (
            "gemini-4-argon-medium  (Gemini 4 Argon (Medium))".to_string(),
            "gemini-4-argon-medium".to_string(),
        ),
        (
            "gemini-4-argon-high  (Gemini 4 Argon (High))".to_string(),
            "gemini-4-argon-high".to_string(),
        ),
        ("plain".to_string(), "plain".to_string()),
    ])
    .unwrap();
    let mut p = crate::menu::Picker {
        stage: crate::menu::PickStage::Agent,
        resume_scope: Default::default(),
        title: "t".into(),
        items: vec![],
        filter: String::new(),
        cursor: 0,
        pending: Some(rx),
        loading: true,
        group_effort: true,
        variants: Default::default(),
    };
    p.poll_pending();
    let values: Vec<_> = p.items.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(values, vec!["gemini-4-argon", "plain"]);
    assert_eq!(p.variants["gemini-4-argon"], vec!["medium", "high"]);
    assert_eq!(p.items[0].0, "gemini-4-argon  (Gemini 4 Argon)");
}

// ---- 附件 (@檔案／圖片)----

fn temp_dir_with(files: &[(&str, &[u8])]) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ertangle-att-{}", ertangle_core::new_id("t")));
    std::fs::create_dir_all(&d).unwrap();
    for (n, c) in files {
        std::fs::write(d.join(n), c).unwrap();
    }
    d
}

#[test]
fn at_tokens_become_attachments_and_unknown_stay() {
    let d = temp_dir_with(&[
        ("notes.txt", b"hello\n"),
        ("pic.png", &[0x89, b'P', b'N', b'G', 1, 2, 3]),
    ]);
    let text = format!(
        "看 @{}/notes.txt 和 @{}/pic.png，另外 @nobody 與 a@b.com",
        d.display(),
        d.display()
    );
    let (body, atts) = crate::attach::extract_attachments(&text, &d).unwrap();
    assert_eq!(atts.len(), 2);
    assert!(body.contains("notes.txt") && body.contains("pic.png"));
    assert!(body.contains("@nobody") && body.contains("a@b.com"));
    assert_eq!(atts[0].mime, "text/plain");
    assert_eq!(atts[0].text.as_deref(), Some("hello\n"));
    assert!(atts[1].is_image() && atts[1].data.is_some());
}

#[test]
fn leading_at_file_is_not_a_peer_message() {
    let d = temp_dir_with(&[("a.txt", b"x")]);
    assert!(crate::attach::leads_with_file(
        &format!("@{}/a.txt 看一下", d.display()),
        &d
    ));
    assert!(!crate::attach::leads_with_file("@worker/a 嗨", &d));
}

#[test]
fn quoted_path_with_spaces_and_trailing_punctuation() {
    let d = temp_dir_with(&[("my file.txt", b"s"), ("b.txt", b"b")]);
    let (_, atts) = crate::attach::extract_attachments(
        &format!(
            "@\"{}/my file.txt\" 與 @{}/b.txt,",
            d.display(),
            d.display()
        ),
        &d,
    )
    .unwrap();
    assert_eq!(atts.len(), 2);
}

#[test]
fn oversized_image_is_rejected_and_binary_is_path_only() {
    let d = temp_dir_with(&[("bin.dat", &[0, 1, 2, 3])]);
    let (_, atts) =
        crate::attach::extract_attachments(&format!("@{}/bin.dat", d.display()), &d).unwrap();
    assert!(atts[0].text.is_none() && atts[0].data.is_none());
    let big = d.join("big.png");
    std::fs::write(
        &big,
        vec![0u8; (ertangle_core::MAX_IMAGE_BYTES + 1) as usize],
    )
    .unwrap();
    assert!(crate::attach::extract_attachments(&format!("@{}", big.display()), &d).is_err());
}

#[test]
fn path_completion_and_pasted_paths() {
    let d = temp_dir_with(&[("alpha.txt", b"1"), ("alpine.md", b"2")]);
    let c = crate::attach::complete_path(&format!("{}/alp", d.display()));
    assert_eq!(c.len(), 2);
    assert!(crate::attach::common_prefix(&c).ends_with("/alp"));
    let p = d.join("alpha.txt");
    assert_eq!(
        crate::attach::pasted_file_ref(&format!("'{}'", p.display())),
        Some(format!("@{} ", p.display()))
    );
    assert_eq!(
        crate::attach::pasted_file_ref(&format!("file://{}", p.display())),
        Some(format!("@{} ", p.display()))
    );
    assert!(crate::attach::pasted_file_ref("not a file").is_none());
}

#[test]
fn fuzzy_search_ranks_basename_hits_first() {
    use crate::attach::fuzzy_score;
    let best = fuzzy_score("main", "src/main.rs").unwrap();
    let worse = fuzzy_score("main", "docs/maintenance/index.md").unwrap();
    assert!(best > worse);
    assert!(fuzzy_score("zzz", "src/main.rs").is_none());
    // 子序列比對：mrs 命中 main.rs
    assert!(fuzzy_score("mrs", "src/main.rs").is_some());
}

#[test]
fn project_search_finds_files_and_resolves_bare_names() {
    let d = temp_dir_with(&[("top.txt", b"t")]);
    std::fs::create_dir_all(d.join("src/deep")).unwrap();
    std::fs::write(d.join("src/deep/needle.rs"), "fn x(){}").unwrap();
    std::fs::write(d.join("src/other.rs"), "").unwrap();
    let hits = crate::attach::suggestions(&d, "needle", 5);
    assert_eq!(hits.first().map(String::as_str), Some("src/deep/needle.rs"));
    // 只打檔名也能附上 (唯一符合)
    let (body, atts) = crate::attach::extract_attachments("看 @needle.rs 吧", &d).unwrap();
    assert_eq!(atts.len(), 1);
    assert_eq!(body, "看 needle.rs 吧");
    // 沒有檔案時不亂猜
    let (_, none) = crate::attach::extract_attachments("@nothing.rs", &d).unwrap();
    assert!(none.is_empty());
}

#[test]
fn current_at_token_reads_text_before_cursor() {
    assert_eq!(
        crate::attach::current_at_token("看 @src/ma", 9),
        Some((2, "src/ma".to_string()))
    );
    assert_eq!(crate::attach::current_at_token("a@b", 3), None);
    assert_eq!(
        crate::attach::current_at_token("看 @a 然後", 4),
        Some((2, "a".to_string()))
    );
}

#[test]
fn help_panel_lists_sections_and_closes() {
    let mut app = sample_app(&["x"]);
    crate::help::open_help(&mut app);
    assert_eq!(app.mode, Mode::Help);
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(
        squash(&text).contains("說明") && text.contains("Space"),
        "{text}"
    );
    crate::help::handle_help_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn header_hides_default_flags_and_shows_deviations() {
    let mut app = sample_app(&["x"]);
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(!text.contains("layout:") && !text.contains("mouse:") && !text.contains("think:"));
    app.show_thoughts = true;
    app.raw_md = true;
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(
        squash(&text).contains("[思考]") && text.contains("[raw]"),
        "{text}"
    );
}

#[test]
fn tool_runs_fold_into_one_line_but_keep_failures() {
    let src: Vec<String> = [
        "hello",
        "▸ Read a.rs",
        "  ✓ ok",
        "▸ Bash cargo test",
        "  ✗ exit 1",
        "▸ Read b.rs",
        "  ✓ ok",
        "▸ Bash ls",
        "  ✓ ok",
        "done",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let folded = crate::app::visible_transcript(src.iter(), false, false, &[]);
    assert_eq!(folded.len(), 5, "{folded:?}");
    assert!(folded[1].contains("工具 ×2"), "{folded:?}");
    assert!(folded.iter().any(|l| l.contains("✗ exit 1")));
    let full = crate::app::visible_transcript(src.iter(), false, true, &[]);
    assert_eq!(full.len(), src.len());
}

#[test]
fn history_recalls_dedups_and_restores_draft() {
    let mut h = crate::history::History::default();
    h.push("one");
    h.push("two");
    h.push("two");
    h.push("   ");
    assert_eq!(h.prev("draft").as_deref(), Some("two"));
    assert_eq!(h.prev("x").as_deref(), Some("one"));
    assert_eq!(h.prev("x").as_deref(), Some("one"), "stays at oldest");
    assert_eq!(h.next().as_deref(), Some("two"));
    assert_eq!(
        h.next().as_deref(),
        Some("draft"),
        "past newest returns the draft"
    );
    assert_eq!(h.next(), None);
}

#[test]
fn history_persists_across_loads() {
    let dir = std::env::temp_dir().join(format!("ertangle-hist-{}", std::process::id()));
    let path = dir.join("h.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    let mut h = crate::history::History::load(Some(path.clone()));
    h.push("第一則 \"quoted\"");
    h.push("second");
    let mut h2 = crate::history::History::load(Some(path));
    assert_eq!(h2.prev("").as_deref(), Some("second"));
    assert_eq!(h2.prev("").as_deref(), Some("第一則 \"quoted\""));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tui_queue_and_unqueue_preserve_original_input_with_at_filename() {
    use std::io::{BufRead, BufReader};
    let d = temp_dir_with(&[("foo.rs", b"fn main() {}\n")]);
    let mut app = sample_app(&[]);
    app.state.sessions.get_mut("ses_test").unwrap().cwd = d.to_string_lossy().to_string();
    let (mut w, peer) = pair_writer();

    // 排入含 `@foo.rs` 的原始文字
    crate::commands::execute_command(&mut app, &mut w, "queue 請檢查 @foo.rs 的實作").unwrap();
    assert_eq!(
        app.state.queues.get("ses_test").map(Vec::as_slice),
        Some(&["請檢查 @foo.rs 的實作".to_string()][..])
    );

    // 模擬 daemon 廣播 QueueChanged，確認 State.queues 儲存原始輸入文字
    app.state.apply(&ertangle_core::Record {
        seq: 10,
        ts: 10,
        event: ertangle_core::Event::QueueChanged {
            session: "ses_test".into(),
            items: vec!["第一則".into(), "請檢查 @foo.rs 的實作".into()],
        },
    });
    crate::commands::execute_command(&mut app, &mut w, "queue").unwrap();
    assert!(app.status.contains("排隊 2 則"), "{}", app.status);

    // `:unqueue` 取回最後一則，app.input 應為含 `@foo.rs` 的原始輸入文字並進入 Mode::Prompt
    crate::commands::execute_command(&mut app, &mut w, "unqueue").unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "請檢查 @foo.rs 的實作");
    assert_eq!(app.input_cursor, app.input.chars().count());
    assert_eq!(
        app.state.queues.get("ses_test").map(Vec::as_slice),
        Some(&["第一則".to_string()][..])
    );

    // `:queue clear` 清空佇列並送出 QueueClear
    crate::commands::execute_command(&mut app, &mut w, "queue clear").unwrap();
    assert!(!app.state.queues.contains_key("ses_test"));

    let mut reader = BufReader::new(peer);
    let mut line1 = String::new();
    let mut line2 = String::new();
    let mut line3 = String::new();
    reader.read_line(&mut line1).unwrap();
    reader.read_line(&mut line2).unwrap();
    reader.read_line(&mut line3).unwrap();
    let req1: Request = ertangle_protocol::decode_line(&line1).unwrap();
    let req2: Request = ertangle_protocol::decode_line(&line2).unwrap();
    let req3: Request = ertangle_protocol::decode_line(&line3).unwrap();
    match req1 {
        Request::Prompt {
            session,
            text,
            attachments,
            queue,
        } => {
            assert_eq!(session, "ses_test");
            assert_eq!(text, "請檢查 foo.rs 的實作");
            assert!(queue);
            assert_eq!(attachments.len(), 1);
            assert_eq!(
                attachments[0].raw_prompt.as_deref(),
                Some("請檢查 @foo.rs 的實作")
            );
        }
        other => panic!("unexpected req1: {other:?}"),
    }
    assert!(matches!(req2, Request::QueuePop { session } if session == "ses_test"));
    assert!(matches!(req3, Request::QueueClear { session } if session == "ses_test"));
}

#[test]
fn inputbox_wraps_and_tracks_cursor() {
    use crate::inputbox::{cursor_pos, index_at, line_bounds, rows};
    // 寬 4：中文字佔 2 欄，每列 2 字
    let t = "一二三四五";
    let r = rows(t, 4);
    assert_eq!(r.len(), 3);
    assert_eq!(cursor_pos(t, &r, 0), (0, 0));
    assert_eq!(
        cursor_pos(t, &r, 2),
        (1, 0),
        "wrap boundary belongs to next row"
    );
    assert_eq!(
        cursor_pos(t, &r, 5),
        (2, 2),
        "end of input stays on the last row"
    );
    // 明確換行
    let t = "ab\ncd\n";
    let r = rows(t, 10);
    assert_eq!(r.len(), 3);
    assert_eq!(
        cursor_pos(t, &r, 2),
        (0, 2),
        "cursor before newline is on the first row"
    );
    assert_eq!(cursor_pos(t, &r, 3), (1, 0));
    assert_eq!(cursor_pos(t, &r, 6), (2, 0));
    assert_eq!(index_at(t, &r, 1, 1), 4);
    assert_eq!(index_at(t, &r, 1, 9), 5, "clamps to row end");
    assert_eq!(line_bounds(t, 4), (3, 5));
    assert_eq!(rows("", 10).len(), 1);
}

#[test]
fn multiline_input_keys_and_paste() {
    let mut app = test_app();
    app.mode = Mode::Prompt;
    app.input_inner_w = 20;
    handle_paste(&mut app, "line1\r\nline2\nline3");
    assert_eq!(app.input, "line1\nline2\nline3");
    // ↑ 在多行內先移動游標，而不是回顧歷史
    app.prompt_history.push("old");
    assert!(crate::input::move_cursor_row(&mut app, true));
    assert!(crate::input::move_cursor_row(&mut app, true));
    assert!(
        !crate::input::move_cursor_row(&mut app, true),
        "top row falls through to history"
    );
    assert!(crate::input::move_cursor_row(&mut app, false));
    // 命令列維持單行：換行轉成空白
    app.mode = Mode::Command;
    app.input.clear();
    app.input_cursor = 0;
    handle_paste(&mut app, "a\nb");
    assert_eq!(app.input, "a b");
}

#[test]
fn scope_filters_sessions_by_project_and_names_disambiguate() {
    let mut app = test_app();
    app.home_project = "/work/alpha".into();
    let mk = |id: &str, cwd: &str, root: Option<&str>| ertangle_core::Session {
        id: id.into(),
        kind: AgentKind::OpenCode,
        label: id.into(),
        cwd: cwd.into(),
        branch: None,
        role: Role::Worker,
        status: SessionStatus::Idle,
        model: None,
        cost: Cost::default(),
        parent: None,
        created_ms: 1,
        transport: "acp".into(),
        status_note: None,
        repo_root: root.map(String::from),
        context: None,
        native_id: None,
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };
    for s in [
        mk("a", "/work/alpha", None),
        mk("b", "/work/alpha-wt/b", Some("/work/alpha")),
        mk("c", "/work/beta", None),
    ] {
        app.state.sessions.insert(s.id.clone(), s);
    }
    assert_eq!(app.session_ids().len(), 3);
    assert_eq!(app.project_count(), 2);
    crate::app::toggle_scope(&mut app, Some(true));
    let ids = app.session_ids();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(!ids.contains(&"c".to_string()));
    let roots = ["/a/app", "/b/app", "/c/x"];
    assert_eq!(crate::app::project_name("/a/app", &roots), "a/app");
    assert_eq!(crate::app::project_name("/c/x", &roots), "x");
}

/// 測試用 App：session 的 cwd 是任意值，所以預設不套用「只看本專案」。
fn test_app() -> App {
    let mut app = App::new();
    app.scope_here = false;
    app
}

#[test]
fn input_box_selection_extracts_text_and_spans_rows() {
    use crate::inputbox::{selected_text, selection_range};
    assert_eq!(selection_range(None), None);
    assert_eq!(selection_range(Some((3, 3))), None, "點一下不算選取");
    assert_eq!(selection_range(Some((2, 5))), Some((2, 6)));
    assert_eq!(selection_range(Some((5, 2))), Some((2, 6)), "反向拖曳");
    assert_eq!(selected_text("你好世界", Some((0, 1))), "你好");
    assert_eq!(selected_text("abc\ndef", Some((2, 4))), "c\nd");
    assert_eq!(selected_text("abc", None), "");
}

#[test]
fn normal_mode_typing_nudges_instead_of_silently_ignoring() {
    let mut app = sample_app(&["hi"]);
    let (mut w, _peer) = pair_writer();
    let key = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
    handle_key(&mut app, &mut w, key).unwrap();
    assert_eq!(app.mode, Mode::Normal, "不該自動進入輸入模式");
    assert!(app.input.is_empty());
    assert!(app.input_nudging(), "應顯示「先按 i 或 Enter」提示");
    assert!(app.status.contains("i") && app.status.contains("Enter"));

    // 按下 i 之後才真的能打字。
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.input, "w");
}

#[test]
fn paste_in_normal_mode_enters_prompt_mode() {
    let mut app = sample_app(&["hi"]);
    handle_paste(&mut app, "hello");
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "hello");
}

#[test]
fn click_and_drag_in_input_box_selects_text() {
    let mut app = sample_app(&["hi"]);
    app.mode = Mode::Prompt;
    app.input = "hello world".into();
    app.input_cursor = 11;
    let _ = render_size(&mut app, 120, 40);
    let area = app.input_area;
    let (mut w, _peer) = pair_writer();

    // 點在輸入框最前面：游標移過去並開始選取。
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, &mut w, down).unwrap();
    assert_eq!(app.input_cursor, 0);
    assert_eq!(app.input_sel, Some((0, 0)));

    // 往右拖到第 5 欄 → 選到 "hello"。
    let drag = MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        ..down
    };
    handle_mouse(
        &mut app,
        &mut w,
        MouseEvent {
            column: area.x + 5,
            ..drag
        },
    )
    .unwrap();
    assert_eq!(
        crate::inputbox::selected_text(&app.input, app.input_sel),
        "hello"
    );
}

#[test]
fn click_on_input_box_in_normal_mode_starts_prompt() {
    let mut app = sample_app(&["hi"]);
    let _ = render_size(&mut app, 120, 40);
    let area = app.input_area;
    let (mut w, _peer) = pair_writer();
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, &mut w, ev).unwrap();
    assert_eq!(app.mode, Mode::Prompt);
}

#[test]
fn all_scope_shows_project_chip_and_groups_by_project() {
    let mut app = sample_app(&["hi"]);
    app.home_project = "/work/alpha".into();
    app.state.sessions.get_mut("ses_test").unwrap().cwd = "/work/beta".into();
    app.scope_here = false;
    let (_, text) = render_size(&mut app, 120, 40);
    let squashed: String = text.split_whitespace().collect();
    assert!(squashed.contains("beta"), "{text}");
    assert!(squashed.contains("Sessions·全部"), "{text}");
}

#[test]
fn shutdown_needs_confirmation_while_sessions_are_live() {
    let mut app = sample_app(&["hi"]);
    let (mut w, mut peer) = pair_writer();
    peer.set_nonblocking(true).unwrap();
    crate::commands::execute_command(&mut app, &mut w, "shutdown").unwrap();
    assert!(!app.should_quit, "live session: must not shut down yet");
    assert!(app.status.contains(":shutdown!"), "{}", app.status);
    let mut buf = [0u8; 16];
    assert!(
        std::io::Read::read(&mut peer, &mut buf).is_err(),
        "nothing sent yet"
    );
    crate::commands::execute_command(&mut app, &mut w, "shutdown!").unwrap();
    assert!(app.should_quit);
}

#[test]
fn stop_only_kills_this_projects_sessions() {
    let mut app = sample_app(&["hi"]);
    app.home_project = "/work/alpha".into();
    app.state.sessions.get_mut("ses_test").unwrap().cwd = "/work/beta".into();
    let (mut w, mut peer) = pair_writer();
    peer.set_nonblocking(true).unwrap();
    crate::commands::execute_command(&mut app, &mut w, "stop").unwrap();
    assert!(app.status.contains('0'), "{}", app.status);
    let mut buf = [0u8; 16];
    assert!(
        std::io::Read::read(&mut peer, &mut buf).is_err(),
        "other project untouched"
    );
}

// ---- T18：任務視圖 ----

fn add_task(
    app: &mut App,
    id: &str,
    title: &str,
    status: ertangle_core::TaskStatus,
    assignee: Option<&str>,
    deps: &[&str],
) {
    app.state.tasks.insert(
        id.into(),
        ertangle_core::Task {
            id: id.into(),
            title: title.into(),
            detail: format!("detail-of-{id}"),
            status,
            assignee: assignee.map(String::from),
            created_by: None,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            updated_ms: 1,
        },
    );
}

fn key(app: &mut App, w: &mut HostRouter, code: KeyCode) {
    handle_key(app, w, KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
}

#[test]
fn tasks_view_groups_by_status_in_every_layout() {
    use ertangle_core::TaskStatus::*;
    let mut app = sample_app(&["hi"]);
    add_task(
        &mut app,
        "tsk_aaa111",
        "alpha-task",
        Doing,
        Some("ses_test"),
        &[],
    );
    add_task(&mut app, "tsk_bbb222", "beta-task", Review, None, &[]);
    add_task(&mut app, "tsk_ccc333", "gamma-task", Todo, None, &[]);
    add_task(
        &mut app,
        "tsk_ddd444",
        "delta-task",
        Blocked,
        None,
        &["tsk_ccc333"],
    );
    add_task(&mut app, "tsk_eee555", "epsilon-task", Done, None, &[]);
    app.view = CenterView::Tasks;

    for layout in [LayoutMode::Three, LayoutMode::Two, LayoutMode::Tabs] {
        app.layout = layout;
        let (_, text) = render_size(&mut app, 120, 40);
        let mut last = 0;
        for group in [
            "doing (1)",
            "review (1)",
            "todo (1)",
            "blocked (1)",
            "done (1)",
        ] {
            let at = text
                .find(group)
                .unwrap_or_else(|| panic!("{layout:?}: 缺 {group}"));
            assert!(at > last, "{layout:?}: {group} 順序不對");
            last = at;
        }
        for title in [
            "alpha-task",
            "beta-task",
            "gamma-task",
            "delta-task",
            "epsilon-task",
        ] {
            assert!(text.contains(title), "{layout:?}: 缺 {title}");
        }
        assert!(text.contains("worker"), "{layout:?}: 缺負責人 label");
        assert!(text.contains("ccc333"), "{layout:?}: 缺相依性");
        assert!(
            text.contains("detail-of-tsk_aaa111"),
            "{layout:?}: 缺選取任務的說明"
        );
    }
    // 標題列只算未完成的任務。
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("4 tasks"), "{text}");
}

#[test]
fn tab_cycles_through_tasks_view_and_clears_click_area() {
    let mut app = sample_app(&["hi"]);
    add_task(
        &mut app,
        "tsk_a",
        "one",
        ertangle_core::TaskStatus::Todo,
        None,
        &[],
    );
    let (mut w, _peer) = pair_writer();
    for expect in [
        CenterView::Diff,
        CenterView::Worktrees,
        CenterView::Tasks,
        CenterView::Transcript,
    ] {
        key(&mut app, &mut w, KeyCode::Tab);
        assert_eq!(app.view, expect);
    }
    execute_command(&mut app, &mut w, "tasks").unwrap();
    assert_eq!(app.view, CenterView::Tasks);
    let _ = render_size(&mut app, 120, 40);
    assert!(app.tasks_area.width > 0);
    app.view = CenterView::Transcript;
    let _ = render_size(&mut app, 120, 40);
    assert_eq!(app.tasks_area, Rect::default(), "沒畫任務視圖時要清空區域");
}

#[test]
fn task_cursor_moves_and_enter_jumps_to_assignee() {
    use ertangle_core::TaskStatus::*;
    let mut app = sample_app(&["hi"]);
    add_session(&mut app, "ses_other", "helper", 2);
    add_task(&mut app, "tsk_a", "first", Doing, Some("ses_test"), &[]);
    add_task(&mut app, "tsk_b", "second", Todo, Some("ses_other"), &[]);
    app.view = CenterView::Tasks;
    let (mut w, _peer) = pair_writer();
    assert_eq!(app.selected_id().as_deref(), Some("ses_test"));

    key(&mut app, &mut w, KeyCode::Char('j'));
    key(&mut app, &mut w, KeyCode::Char('j'));
    assert_eq!(app.task_cursor, 1, "游標不超過最後一個任務");
    assert_eq!(app.selected, 0, "任務視圖的 j/k 不動 session 選取");
    key(&mut app, &mut w, KeyCode::Char('k'));
    assert_eq!(app.task_cursor, 0);

    // 滑鼠滾輪也移動任務游標。
    let _ = render_size(&mut app, 120, 40);
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 50,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, &mut w, wheel).unwrap();
    assert_eq!(app.task_cursor, 1);

    key(&mut app, &mut w, KeyCode::Enter);
    assert_eq!(app.selected_id().as_deref(), Some("ses_other"));
    assert_eq!(app.view, CenterView::Transcript);
    assert_eq!(app.mode, Mode::Normal, "Enter 在任務視圖不該開 prompt");
}

#[test]
fn task_status_command_updates_selected_task() {
    use std::io::Read;
    let mut app = sample_app(&["hi"]);
    add_task(
        &mut app,
        "tsk_a",
        "first",
        ertangle_core::TaskStatus::Todo,
        None,
        &[],
    );
    let (mut w, mut peer) = pair_writer();
    let mut buf = [0u8; 512];

    execute_command(&mut app, &mut w, "task-status done").unwrap();
    assert!(peer.read(&mut buf).is_err(), "不在任務視圖時不送出");

    app.view = CenterView::Tasks;
    execute_command(&mut app, &mut w, "task-status nonsense").unwrap();
    assert!(peer.read(&mut buf).is_err(), "狀態無效時不送出");

    execute_command(&mut app, &mut w, "task-status done").unwrap();
    let n = peer.read(&mut buf).unwrap();
    let sent = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(sent.contains("tsk_a") && sent.contains("done"), "{sent}");
    assert!(COMMANDS.iter().any(|(n, _)| *n == "task-status"));
}

#[test]
fn scope_here_hides_tasks_of_other_projects() {
    use ertangle_core::TaskStatus::Todo;
    let mut app = sample_app(&["hi"]);
    app.home_project = "/work/alpha".into();
    app.state.sessions.get_mut("ses_test").unwrap().cwd = "/work/alpha".into();
    add_session(&mut app, "ses_beta", "beta-worker", 2);
    app.state.sessions.get_mut("ses_beta").unwrap().cwd = "/work/beta".into();
    add_task(&mut app, "tsk_a", "mine-task", Todo, Some("ses_test"), &[]);
    add_task(
        &mut app,
        "tsk_b",
        "theirs-task",
        Todo,
        Some("ses_beta"),
        &[],
    );
    add_task(&mut app, "tsk_c", "orphan-task", Todo, None, &[]);
    // 建立者在別的專案、還沒指派：也算別的專案。
    add_task(&mut app, "tsk_d", "theirs-created", Todo, None, &[]);
    app.state.tasks.get_mut("tsk_d").unwrap().created_by = Some("ses_beta".into());
    app.view = CenterView::Tasks;
    // Bus 欄的精簡清單不分專案，用兩欄版面只看任務視圖。
    app.layout = LayoutMode::Two;

    app.scope_here = true;
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(
        text.contains("mine-task") && text.contains("orphan-task"),
        "{text}"
    );
    assert!(
        !text.contains("theirs-task") && !text.contains("theirs-created"),
        "{text}"
    );

    app.scope_here = false;
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("theirs-task") && text.contains("theirs-created"));
}

// ---- T16：在逐字稿與 diff 裡搜尋 ----

fn kev(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn type_keys(app: &mut App, w: &mut HostRouter, text: &str) {
    for c in text.chars() {
        handle_key(app, w, kev(KeyCode::Char(c))).unwrap();
    }
}

/// 60 行逐字稿 (畫面一次放不下)，第 5、20、41 行含 `needle`，第 30 行含 `Needle`。
fn search_app() -> App {
    let lines: Vec<String> = (0..60)
        .map(|i| match i {
            5 | 20 | 41 => format!("row {i} needle here"),
            30 => format!("row {i} Needle Upper"),
            _ => format!("row {i} filler"),
        })
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut app = sample_app(&refs);
    app.raw_md = true; // 一行來源對一行顯示，行號好對
    render_size(&mut app, 120, 30); // 先建立 md_cache，後續設定 transcript_scroll 才不會被首繪重置
    app
}

fn status_row(app: &mut App) -> String {
    let (term, _) = render_size(app, 120, 30);
    squash(&row_text(&term, 29))
}

#[test]
fn search_jumps_to_first_match_and_shows_count() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(0);
    render_size(&mut app, 120, 30);

    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    assert_eq!(app.mode, Mode::Search);
    let (_, text) = render_size(&mut app, 120, 30);
    assert!(squash(&text).contains("搜尋"), "輸入框標題要顯示搜尋");

    type_keys(&mut app, &mut w, "needle");
    assert_eq!(app.transcript_scroll, Some(5), "捲到第一個符合的行");
    let s = app.search.as_ref().unwrap();
    assert_eq!(s.matches, vec![5, 20, 30, 41], "全小寫查詢不分大小寫");
    assert!(status_row(&mut app).contains("1/4"));
    assert_eq!(app.transcript_offset, 5, "畫面第一行就是符合項");

    // 目前這一個與其他符合項用不同底色，沒符合的行不加底色。
    let (term, _) = render_size(&mut app, 120, 30);
    let buf = term.backend().buffer();
    let x = app.transcript_area.x + 1;
    let y = app.transcript_area.y + 1;
    assert_eq!(buf[(x, y)].bg, Color::Yellow);
    assert_eq!(buf[(x, y + 1)].bg, Color::Reset);
    assert_eq!(buf[(x, y + 15)].bg, Color::Rgb(70, 70, 120));
}

#[test]
fn search_enter_cycles_forward_and_up_goes_back() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(0);
    render_size(&mut app, 120, 30);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "needle");

    let mut seen = vec![app.transcript_scroll];
    for _ in 0..4 {
        handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
        seen.push(app.transcript_scroll);
    }
    assert_eq!(
        seen,
        vec![Some(5), Some(20), Some(30), Some(41), Some(5)],
        "最後一個之後繞回第一個"
    );
    assert!(status_row(&mut app).contains("1/4"));

    handle_key(&mut app, &mut w, kev(KeyCode::Up)).unwrap();
    assert_eq!(
        app.transcript_scroll,
        Some(41),
        "↑ 從第一個反向繞到最後一個"
    );
    assert!(status_row(&mut app).contains("4/4"));
    handle_key(&mut app, &mut w, kev(KeyCode::Up)).unwrap();
    assert_eq!(app.transcript_scroll, Some(30));
    handle_key(&mut app, &mut w, kev(KeyCode::Down)).unwrap();
    assert_eq!(app.transcript_scroll, Some(41), "↓ 等同 Enter");
}

#[test]
fn search_starts_from_current_position() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(10);
    render_size(&mut app, 120, 30);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "needle");
    assert_eq!(app.transcript_scroll, Some(20), "從目前位置往後找");
    assert!(status_row(&mut app).contains("2/4"));
}

#[test]
fn search_is_smartcase() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    render_size(&mut app, 120, 30);

    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "upper");
    assert_eq!(
        app.search.as_ref().unwrap().matches,
        vec![30],
        "全小寫查詢找得到大寫內容"
    );
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();

    execute_command(&mut app, &mut w, "find Needle").unwrap();
    assert_eq!(app.mode, Mode::Search, ":find 進入搜尋模式");
    assert_eq!(
        app.search.as_ref().unwrap().matches,
        vec![30],
        "含大寫的查詢區分大小寫"
    );
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();

    execute_command(&mut app, &mut w, "find UPPER").unwrap();
    assert!(app.search.as_ref().unwrap().matches.is_empty());
    assert!(status_row(&mut app).contains("找不到"));
}

#[test]
fn search_without_match_keeps_scroll_position() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(12);
    render_size(&mut app, 120, 30);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "zzz-not-there");
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Up)).unwrap();
    assert_eq!(app.transcript_scroll, Some(12));
    assert!(status_row(&mut app).contains("找不到"));
    assert_eq!(app.transcript_offset, 12);

    // 跟隨最新時找不到，也不會變成暫停跟隨。
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();
    app.transcript_scroll = None;
    render_size(&mut app, 120, 30);
    execute_command(&mut app, &mut w, "find zzz-not-there").unwrap();
    assert_eq!(app.transcript_scroll, None);
}

#[test]
fn search_updates_matches_on_new_output_without_moving() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(0);
    render_size(&mut app, 120, 30);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "needle");
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    render_size(&mut app, 120, 30);
    assert_eq!(app.transcript_scroll, Some(41));
    let off = app.transcript_offset;

    let buf = app.state.transcripts.get_mut("ses_test").unwrap();
    for i in 60..120 {
        buf.push_back(if i == 100 {
            "row 100 needle late".to_string()
        } else {
            format!("row {i} filler")
        });
    }
    // 跳過串流節流，讓這次繪製就重算。
    app.last_rebuild = std::time::Instant::now() - MD_REBUILD_INTERVAL * 2;
    assert!(
        status_row(&mut app).contains("4/5"),
        "新輸出裡的符合項要算進來"
    );
    let s = app.search.as_ref().unwrap();
    assert_eq!(s.matches, vec![5, 20, 30, 41, 100]);
    assert_eq!(s.current_line(), Some(41), "目前位置不會跳走");
    assert_eq!(app.transcript_scroll, Some(41));
    assert_eq!(app.transcript_offset, 41);
    assert!(off <= 41);
}

#[test]
fn search_near_bottom_is_not_scrolled_away_by_new_output() {
    let mut app = search_app();
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(25);
    render_size(&mut app, 120, 30);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "needle");
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    // 第 41 行在最後一頁裡：畫面停在最底，但不恢復跟隨。
    render_size(&mut app, 120, 30);
    assert_eq!(app.transcript_scroll, Some(41));

    let buf = app.state.transcripts.get_mut("ses_test").unwrap();
    for i in 60..120 {
        buf.push_back(format!("row {i} filler"));
    }
    app.last_rebuild = std::time::Instant::now() - MD_REBUILD_INTERVAL * 2;
    let (_, text) = render_size(&mut app, 120, 30);
    assert_eq!(app.transcript_offset, 41);
    assert!(text.contains("row 41 needle here"));
}

#[test]
fn search_in_diff_view_moves_cursor() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.view = CenterView::Diff;
    app.diff_lines = vec![
        "diff --git a/f b/f".into(),
        "@@ -1,2 +1,3 @@".into(),
        " ctx".into(),
        "-old Target".into(),
        "+new target".into(),
        "+extra".into(),
    ];
    app.diff_gutter = build_gutter(&app.diff_lines);
    render(&mut app);

    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    type_keys(&mut app, &mut w, "target");
    assert_eq!(app.diff_cursor, 3);
    assert!(status_row(&mut app).contains("1/2"));
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.diff_cursor, 4);
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.diff_cursor, 3, "繞回第一個");
    assert_eq!(app.transcript_scroll, None, "不該動到逐字稿的捲動");

    // diff 重新載入：符合項跟著重算。
    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Diff {
            session: "ses_test".into(),
            branch: None,
            cwd: "/tmp".into(),
            diff: "+only target\n ctx".into(),
        },
    );
    assert_eq!(app.search.as_ref().unwrap().matches, vec![0]);
}

#[test]
fn search_escape_restores_normal_keys_and_remembers_query() {
    let mut app = search_app();
    add_session(&mut app, "ses_b", "second", 2);
    let (mut w, _peer) = pair_writer();
    app.transcript_scroll = Some(0);
    render_size(&mut app, 120, 30);
    assert_eq!(app.selected, 0);

    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    // 搜尋中 j／k／n／q 是查詢文字，不是 Normal 按鍵。
    type_keys(&mut app, &mut w, "jknq");
    assert_eq!(app.input, "jknq");
    assert_eq!((app.selected, app.mode), (0, Mode::Search));
    assert!(!app.should_quit);
    for _ in 0..4 {
        handle_key(&mut app, &mut w, kev(KeyCode::Backspace)).unwrap();
    }
    type_keys(&mut app, &mut w, "needle");
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.transcript_scroll, Some(20));

    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.search.is_none());
    assert!(app.input.is_empty());
    assert_eq!(app.transcript_scroll, Some(20), "Esc 停在目前位置");
    assert!(app.status.contains("2/4"));

    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.selected, 1, "j 恢復選下一個 session");
    handle_key(&mut app, &mut w, kev(KeyCode::Char('k'))).unwrap();
    assert_eq!(app.selected, 0);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('n'))).unwrap();
    assert_eq!(app.mode, Mode::Pick, "n 恢復建立 session");
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();
    assert_eq!(app.mode, Mode::Normal);

    // 再按 f 預填上一次的查詢。
    render_size(&mut app, 120, 30);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    assert_eq!(app.input, "needle");
    assert_eq!(app.search.as_ref().unwrap().matches.len(), 4);
}

#[test]
fn search_skips_hidden_lines_and_is_documented() {
    let mut app = sample_app(&["💭 secret thought", "visible secret", "plain"]);
    let (mut w, _peer) = pair_writer();
    render(&mut app);
    execute_command(&mut app, &mut w, "find secret").unwrap();
    assert_eq!(
        app.search.as_ref().unwrap().matches.len(),
        1,
        "隱藏的思考行不在搜尋範圍"
    );
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();

    // Worktrees 視圖不支援搜尋。
    app.view = CenterView::Worktrees;
    handle_key(&mut app, &mut w, kev(KeyCode::Char('f'))).unwrap();
    assert_eq!(app.mode, Mode::Normal);

    assert!(COMMANDS.iter().any(|(n, _)| *n == "find"));
    let help = crate::help::help_lines();
    assert!(help.iter().any(|(_, k, _)| k == "f"));
    assert!(help.iter().any(|(_, k, _)| k == ":find"));
}

#[test]
fn resume_command_sends_request_and_is_discoverable() {
    let mut app = sample_app(&["hi"]);
    let (mut w, mut peer) = pair_writer();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    crate::commands::execute_command(&mut app, &mut w, "resume 繼續做").unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut peer), &mut line).unwrap();
    let req: Request = serde_json::from_str(line.trim()).unwrap();
    match req {
        Request::Resume { session, prompt } => {
            assert_eq!(session, "ses_test");
            assert_eq!(prompt.as_deref(), Some("繼續做"));
        }
        other => panic!("應送出 Resume，得到 {other:?}"),
    }

    // 不帶參數且無 ended session 時不開選擇器並設定狀態提示。
    crate::commands::execute_command(&mut app, &mut w, "resume").unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.status.contains("沒有可接續的 session"));

    // 有 ended session 時，不帶參數開啟選擇器。
    app.state.sessions.get_mut("ses_test").unwrap().status = SessionStatus::Exited;
    crate::commands::execute_command(&mut app, &mut w, "resume").unwrap();
    assert_eq!(app.mode, Mode::Pick);
    assert!(app.picker.is_some());

    // Tab 自動完成表與 Space 選單都找得到，且 session 層的快捷鍵不重複。
    assert!(crate::commands::COMMANDS
        .iter()
        .any(|(n, _)| *n == "resume"));
    let (_, items) = crate::menu::menu_items(&['s']).unwrap();
    assert!(items.iter().any(|i| i.key == 'r'));
    let mut keys: Vec<char> = items.iter().map(|i| i.key).collect();
    keys.sort_unstable();
    let n = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), n, "Space s 選單有重複的快捷鍵");
}

#[test]
fn format_compact_age_produces_compact_strings() {
    use crate::menu::format_compact_age;
    assert_eq!(format_compact_age(10_000, 10_000), "0m");
    assert_eq!(format_compact_age(70_000, 10_000), "1m");
    assert_eq!(format_compact_age(3_600_000 + 10_000, 10_000), "1h");
    assert_eq!(format_compact_age(86_400_000 * 2 + 10_000, 10_000), "2d");
}

#[test]
fn resume_picker_lists_ended_filters_and_resumes() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();

    let now = ertangle_core::now_millis();

    // 1. 執行中的 session (live)：不應出現在清單中
    let s_live = Session {
        id: "ses_live".into(),
        kind: AgentKind::OpenCode,
        label: "worker/live".into(),
        cwd: "/tmp".into(),
        branch: None,
        role: Role::Worker,
        status: SessionStatus::Busy,
        model: Some("gpt-4".into()),
        cost: Cost::default(),
        parent: None,
        created_ms: now.saturating_sub(10_000),
        transport: "acp".into(),
        status_note: None,
        repo_root: None,
        context: None,
        native_id: Some("nav_live".into()),
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };

    // 2. 舊的已結束 session，具備 native_id (可接續)
    let s_old = Session {
        id: "ses_old".into(),
        kind: AgentKind::ClaudeCode,
        label: "worker/alpha".into(),
        cwd: "/tmp".into(),
        branch: None,
        role: Role::Worker,
        status: SessionStatus::Exited,
        model: Some("sonnet".into()),
        cost: Cost::default(),
        parent: None,
        created_ms: now.saturating_sub(200_000),
        transport: "acp".into(),
        status_note: None,
        repo_root: None,
        context: None,
        native_id: Some("nav_old".into()),
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };

    // 3. 新的失敗 session，沒有 native_id (不可接續)
    let s_new = Session {
        id: "ses_new".into(),
        kind: AgentKind::Antigravity,
        label: "worker/beta".into(),
        cwd: "/tmp".into(),
        branch: None,
        role: Role::Worker,
        status: SessionStatus::Failed,
        model: None,
        cost: Cost::default(),
        parent: None,
        created_ms: now.saturating_sub(50_000),
        transport: "acp".into(),
        status_note: None,
        repo_root: None,
        context: None,
        native_id: None,
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };

    app.state.sessions.insert("ses_live".into(), s_live);
    app.state.sessions.insert("ses_old".into(), s_old);
    app.state.sessions.insert("ses_new".into(), s_new);
    app.scope_here = true;
    app.home_project = "/tmp".into();

    crate::menu::start_resume_picker(&mut app);
    assert_eq!(app.mode, Mode::Pick);
    let picker = app.picker.as_ref().unwrap();
    // 只列出已結束 (Exited 或 Failed) 的 session，共 2 個。
    assert_eq!(picker.items.len(), 2);
    // 最新的排在前面 (ses_new 在前，ses_old 在後)。
    assert_eq!(picker.items[0].1, "ses_new");
    assert_eq!(picker.items[1].1, "ses_old");

    // 檢查標籤格式：
    // ses_new: Failed 顯示 ✗，無 model 則只顯示 kind，無 native_id 結尾為 –
    assert!(picker.items[0]
        .0
        .starts_with("✗ worker/beta  antigravity  "));
    assert!(picker.items[0].0.ends_with("  –"));

    // ses_old: Exited 顯示 ✓，model 為 sonnet，有 native_id 結尾為 ↻
    assert!(picker.items[1]
        .0
        .starts_with("✓ worker/alpha  claude:sonnet  "));

    // 若左側已選中某個可接續的 session，開啟 popup 時游標應預設停在該項。
    let old_idx = app
        .session_ids()
        .iter()
        .position(|id| id == "ses_old")
        .unwrap();
    app.selected = old_idx;
    crate::menu::start_resume_picker(&mut app);
    let picker = app.picker.as_ref().unwrap();
    assert_eq!(picker.cursor, 1);
    assert_eq!(picker.items[picker.cursor].1, "ses_old");
    assert!(picker.items[1].0.ends_with("  ↻"));

    // 測試輸入過濾 (typing filter)
    handle_key(&mut app, &mut w, kev(KeyCode::Char('a'))).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Char('l'))).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Char('p'))).unwrap();
    {
        let p = app.picker.as_ref().unwrap();
        assert_eq!(p.visible().len(), 1);
        assert_eq!(p.visible()[0].1, "ses_old");
    }

    // 按 Backspace 清空過濾
    handle_key(&mut app, &mut w, kev(KeyCode::Backspace)).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Backspace)).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Backspace)).unwrap();
    assert_eq!(app.picker.as_ref().unwrap().visible().len(), 2);

    // 游標目前在 index 0 (ses_new，不可接續)。按 Enter：
    // 不會送出 Resume，選擇器保持開啟，狀態顯示不可接續提示。
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.mode, Mode::Pick);
    assert!(app.picker.is_some());
    assert!(app.status.contains("沒有可接續的對話 id"));

    // 移動游標到 index 1 (ses_old，可接續)。按 Enter：
    // 送出 Request::Resume，選擇器關閉，狀態提示已要求接續。
    handle_key(&mut app, &mut w, kev(KeyCode::Down)).unwrap();
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.picker.is_none());
    assert!(app.status.contains("已要求接續 worker/alpha"));

    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut peer), &mut line).unwrap();
    let req: Request = serde_json::from_str(line.trim()).unwrap();
    match req {
        Request::Resume { session, prompt } => {
            assert_eq!(session, "ses_old");
            assert_eq!(prompt, None);
        }
        other => panic!("應送出 Resume，得到 {other:?}"),
    }

    // 4. 將 ses_old 移除 (`SessionRemoved`) 後，應進入 archived_sessions，且仍可在選擇器與 `:resume` 找到並接續
    app.state.apply(&ertangle_core::Record {
        seq: 10,
        ts: now,
        event: ertangle_core::Event::SessionRemoved {
            session: "ses_old".into(),
        },
    });
    assert!(!app.state.sessions.contains_key("ses_old"));
    assert!(app.state.archived_sessions.contains_key("ses_old"));

    crate::menu::start_resume_picker(&mut app);
    let picker = app.picker.as_ref().unwrap();
    assert!(
        picker
            .items
            .iter()
            .any(|(lbl, id)| id == "ses_old" && lbl.contains("[封存]") && lbl.ends_with("  ↻")),
        "已移除的歷史 session 應出現在選擇器中：{:?}",
        picker.items
    );
    app.picker = None;
    app.mode = Mode::Normal;

    // 支援直接 `:resume worker/alpha 繼續修 bug`
    execute_command(&mut app, &mut w, "resume worker/alpha 繼續修 bug").unwrap();
    assert_eq!(app.pending_select.as_deref(), Some("ses_old"));
    let mut line2 = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut peer), &mut line2).unwrap();
    let req2: Request = serde_json::from_str(line2.trim()).unwrap();
    match req2 {
        Request::Resume { session, prompt } => {
            assert_eq!(session, "ses_old");
            assert_eq!(prompt.as_deref(), Some("繼續修 bug"));
        }
        other => panic!("應送出封存 session 的 Resume，得到 {other:?}"),
    }
}

#[test]
fn budget_command_and_session_list_budget_display() {
    use std::io::BufRead;
    let mut app = sample_app(&["hi"]);
    app.home_project = "/tmp".into();
    app.scope_here = true;
    {
        let s = app.state.sessions.get_mut("ses_test").unwrap();
        s.label = "w".into();
        s.branch = None;
        s.cost.usd = 1.20;
        s.budget_usd = Some(5.0);
    }
    let text = render(&mut app);
    assert!(text.contains("$1.20/$5"), "應顯示 $1.20/$5：{text}");

    let (mut w, peer) = pair_writer();
    crate::commands::execute_command(&mut app, &mut w, "budget 10").unwrap();
    assert!(app.status.contains("$10"), "{}", app.status);
    let mut line = String::new();
    std::io::BufReader::new(peer).read_line(&mut line).unwrap();
    let req: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    match req {
        Request::SessionBudget {
            session,
            budget_usd,
        } => {
            assert_eq!(session, "ses_test");
            assert_eq!(budget_usd, 10.0);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn hints_include_selected_sessions_native_commands() {
    let mut app = sample_app(&[]);
    app.state.apply(&ertangle_core::Record {
        seq: 1,
        ts: 1,
        event: ertangle_core::Event::SessionCommands {
            session: "ses_test".into(),
            commands: vec!["compact".into(), "context".into(), "model".into()],
        },
    });
    app.selected = 0;
    app.input = "co".into();
    let h = crate::commands::hints_with_native(&app);
    let names: Vec<&str> = h.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"compact") && names.contains(&"context"),
        "{names:?}"
    );
    // 與 TUI 命令同名的不重複列出
    app.input = "mod".into();
    let h = crate::commands::hints_with_native(&app);
    assert_eq!(h.iter().filter(|(n, _)| n == "model").count(), 1);

    // Tab 自動完成和提示浮層用同一份候選：agent 原生指令也能補完
    app.input = "compa".into();
    crate::input::complete_command_for_test(&mut app);
    assert_eq!(app.input, "compact ");
    // `/`、`:` 前綴與前導空白保留，照樣補完
    app.input = "/compa".into();
    crate::input::complete_command_for_test(&mut app);
    assert_eq!(app.input, "/compact ");
    app.input = " :conte".into();
    crate::input::complete_command_for_test(&mut app);
    assert_eq!(app.input, " :context ");
    // 多個候選：補到共同前綴
    app.input = "/co".into();
    crate::input::complete_command_for_test(&mut app);
    assert!(app.input.starts_with("/co"), "{}", app.input);
    assert!(app.status.contains("compact"), "{}", app.status);
}

#[test]
fn set_command_offers_argument_candidates() {
    let mut app = sample_app(&[]);
    app.selected = 0;
    let cands = |app: &App, i: &str| -> Vec<String> {
        crate::commands::arg_candidates(app, i)
            .map(|(_, c)| c.into_iter().map(|(v, _)| v).collect())
            .unwrap_or_default()
    };
    let keys = cands(&app, "set ");
    assert!(keys.contains(&"permission".to_string()) && keys.contains(&"--repo".to_string()));
    assert_eq!(cands(&app, "set --repo pe"), vec!["permission"]);
    assert_eq!(
        cands(&app, "set permission "),
        vec!["ask", "auto", "bypass"]
    );
    assert_eq!(cands(&app, "set effort hi"), vec!["high"]);
    assert_eq!(cands(&app, "effort zz"), Vec::<String>::new());
    assert!(cands(&app, "set agent c").contains(&"claude".to_string()));

    // Tab 自動完成：唯一候選補完並加空白
    app.input = "set perm".into();
    crate::input::complete_command_for_test(&mut app);
    assert_eq!(app.input, "set permission ");
    app.input = "/set perm".into();
    crate::input::complete_command_for_test(&mut app);
    assert_eq!(app.input, "/set permission ");
}

#[test]
fn settings_panel_cycles_values_and_writes_config() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let dir = std::env::temp_dir().join(format!("ertangle-settings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);

    let mut app = sample_app(&[]);
    app.selected = 0;
    crate::settings::open(&mut app);
    assert_eq!(app.mode, Mode::Settings);
    // 寬字元在 buffer 中夾著空白格，比對前去掉空白。
    let text: String = render(&mut app).split_whitespace().collect();
    assert!(text.contains("預設權限") && text.contains("獨立worktree"));

    // 第 2 列「預設角色」：未設定 → worker
    let role_idx = crate::settings::ITEMS
        .iter()
        .position(|i| *i == crate::settings::Item::DefaultRole)
        .unwrap();
    app.settings.as_mut().unwrap().cursor = role_idx;
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));
    assert!(app.settings.as_ref().unwrap().sub.is_some());
    for c in "worker".chars() {
        crate::settings::handle_key(&mut app, None, key(KeyCode::Char(c)));
    }
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));
    let cfg = ertangle_daemon::config::Config::load_file(&dir.join("ertangle").join("config.toml"));
    assert_eq!(cfg.defaults.role.as_deref(), Some("worker"));

    // 「預設權限」：未設定 → ask
    let perm_idx = crate::settings::ITEMS
        .iter()
        .position(|i| *i == crate::settings::Item::DefaultPermission)
        .unwrap();
    app.settings.as_mut().unwrap().cursor = perm_idx;
    // Enter 開清單，打字過濾後 Enter 選定
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));
    assert!(app.settings.as_ref().unwrap().sub.is_some());
    let t: String = render(&mut app).split_whitespace().collect();
    assert!(t.contains("過濾"), "{t}");
    for c in "ask".chars() {
        crate::settings::handle_key(&mut app, None, key(KeyCode::Char(c)));
    }
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));
    assert!(app.settings.as_ref().unwrap().sub.is_none());
    // 「worktree」：預設 on → off (寫成布林)
    let wt_idx = crate::settings::ITEMS
        .iter()
        .position(|i| *i == crate::settings::Item::SpawnWorktree)
        .unwrap();
    app.settings.as_mut().unwrap().cursor = wt_idx;
    crate::settings::handle_key(&mut app, None, key(KeyCode::Char(' ')));

    let cfg = ertangle_daemon::config::Config::load_file(&dir.join("ertangle").join("config.toml"));
    assert_eq!(cfg.defaults.permission.as_deref(), Some("ask"));
    assert!(!cfg.agent_spawn.worktree());

    // 再切一次回 on；Esc 關閉
    crate::settings::handle_key(&mut app, None, key(KeyCode::Left));
    let cfg = ertangle_daemon::config::Config::load_file(&dir.join("ertangle").join("config.toml"));
    assert!(cfg.agent_spawn.worktree());
    // 測試 WebCompanion 項目：改為 127.0.0.1:7421
    let web_idx = crate::settings::ITEMS
        .iter()
        .position(|i| *i == crate::settings::Item::WebCompanion)
        .unwrap();
    app.settings.as_mut().unwrap().cursor = web_idx;
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));
    assert!(app.settings.as_ref().unwrap().sub.is_some());
    for c in "127".chars() {
        crate::settings::handle_key(&mut app, None, key(KeyCode::Char(c)));
    }
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));

    let cfg = ertangle_daemon::config::Config::load_file(&dir.join("ertangle").join("config.toml"));
    assert_eq!(cfg.web.enabled, Some(true));
    assert_eq!(cfg.web.addr.as_deref(), Some("127.0.0.1:7421"));

    // 測試 WebPort 項目：改為 8080
    let port_idx = crate::settings::ITEMS
        .iter()
        .position(|i| *i == crate::settings::Item::WebPort)
        .unwrap();
    app.settings.as_mut().unwrap().cursor = port_idx;
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));
    assert!(app.settings.as_ref().unwrap().sub.is_some());
    for c in "8080".chars() {
        crate::settings::handle_key(&mut app, None, key(KeyCode::Char(c)));
    }
    crate::settings::handle_key(&mut app, None, key(KeyCode::Enter));

    let cfg = ertangle_daemon::config::Config::load_file(&dir.join("ertangle").join("config.toml"));
    assert_eq!(cfg.web.port, Some(8080));
    assert_eq!(cfg.web.resolved_port(), 8080);
    assert_eq!(cfg.web.resolved_addr(), "127.0.0.1:8080");

    crate::settings::handle_key(&mut app, None, key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn agy_model_variants_are_split_from_effort() {
    let out = "gemini-4-argon-medium\tGemini 4 Argon (Medium)\n\
gemini-4-argon-high\tGemini 4 Argon (High)\n\
gemini-pro-orchestrator-g1-pro\tPro\n\
claude-opus-5-5-low\tClaude Opus 5.5 (Low)\n\
claude-opus-5-5-high\tClaude Opus 5.5 (High)\n\
gpt-oss-120b-medium\tGPT OSS\n";
    let m = ertangle_adapter::models::split_effort_variants(
        ertangle_adapter::models::parse_models(out),
    );
    let ids: Vec<&str> = m.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "gemini-4-argon",
            "gemini-pro-orchestrator-g1-pro",
            "claude-opus-5-5",
            "gpt-oss-120b-medium"
        ]
    );
    assert!(
        m[0].0.contains("Gemini 4 Argon") && !m[0].0.contains("Medium"),
        "{:?}",
        m[0]
    );
}

#[test]
fn multi_host_single_mode_has_no_prefixes_and_no_host_dots() {
    let mut app = sample_app(&["ses_1"]);
    assert!(!app.has_multiple_hosts());
    let buf = render(&mut app);
    // 單機模式下不得出現 [local] 前綴或主機燈號
    assert!(!buf.contains("[local]"), "單機模式不應顯示 [local]");
    assert!(!buf.contains("● local"), "單機模式標題列不應顯示主機指示燈");
}

#[test]
fn multi_host_aggregated_sessions_and_filter_cycling() {
    let mut app = App::new();

    // 建立 local 與 remote_box 兩台主機資料
    let mut local_data = crate::app::HostData::new("local");
    local_data.status = crate::hosts::HostStatus::Connected;
    let mut s1 = sample_app(&[]).state.sessions["ses_test"].clone();
    s1.id = "ses_local_1".into();
    s1.label = "local_worker".into();
    s1.created_ms = 100;
    local_data.state.sessions.insert("ses_local_1".into(), s1);

    let mut remote_data = crate::app::HostData::new("remote_box");
    remote_data.status = crate::hosts::HostStatus::Connected;
    let mut s2 = sample_app(&[]).state.sessions["ses_test"].clone();
    s2.id = "ses_remote_1".into();
    s2.label = "remote_api".into();
    s2.created_ms = 200;
    remote_data.state.sessions.insert("ses_remote_1".into(), s2);

    app.hosts.insert("local".into(), local_data);
    app.hosts.insert("remote_box".into(), remote_data);
    app.state = app.hosts["local"].state.clone();
    app.scope_here = false;

    assert!(app.has_multiple_hosts());

    // 1. 全部主機檢視：應包含兩台機器的 session
    let vis = app.visible_sessions();
    assert_eq!(vis.len(), 2);
    assert_eq!(vis[0], ("local".to_string(), "ses_local_1".into()));
    assert_eq!(vis[1], ("remote_box".to_string(), "ses_remote_1".into()));

    // 2. 循環篩選主機：全部 -> local -> remote_box -> 全部
    app.cycle_host_filter();
    assert_eq!(app.host_filter, Some("local".to_string()));
    let vis_local = app.visible_sessions();
    assert_eq!(vis_local.len(), 1);
    assert_eq!(vis_local[0].1, "ses_local_1");

    app.cycle_host_filter();
    assert_eq!(app.host_filter, Some("remote_box".to_string()));
    let vis_remote = app.visible_sessions();
    assert_eq!(vis_remote.len(), 1);
    assert_eq!(vis_remote[0].1, "ses_remote_1");

    app.cycle_host_filter();
    assert_eq!(app.host_filter, None);
    assert_eq!(app.visible_sessions().len(), 2);

    // 3. 畫面繪製：應帶有主機前綴與標題列指示燈
    let buf = render(&mut app);
    assert!(buf.contains("[local]"), "多主機畫面應有 [local] 前綴");
    assert!(
        buf.contains("[remote_box]"),
        "多主機畫面應有 [remote_box] 前綴"
    );
    assert!(buf.contains("● local"), "標題列應顯示 local 連線燈號");
    assert!(
        buf.contains("● remote_box"),
        "標題列應顯示 remote_box 連線燈號"
    );
}

#[test]
fn multi_host_router_routes_session_requests_to_owning_host() {
    use std::io::Read;

    let (local_w, mut local_r) = std::os::unix::net::UnixStream::pair().unwrap();
    let (remote_w, mut remote_r) = std::os::unix::net::UnixStream::pair().unwrap();
    local_r.set_nonblocking(true).unwrap();
    remote_r.set_nonblocking(true).unwrap();

    let mut router = crate::hosts::HostRouter::single(ertangle_daemon::ConnStream::Unix(local_w));
    router.set_writer("remote_box", ertangle_daemon::ConnStream::Unix(remote_w));

    let mut app = App::new();
    let mut h1 = crate::app::HostData::new("local");
    let mut s1 = sample_app(&[]).state.sessions["ses_test"].clone();
    s1.id = "ses_loc".into();
    h1.state.sessions.insert("ses_loc".into(), s1);

    let mut h2 = crate::app::HostData::new("remote_box");
    let mut s2 = sample_app(&[]).state.sessions["ses_test"].clone();
    s2.id = "ses_rem".into();
    h2.state.sessions.insert("ses_rem".into(), s2);

    app.hosts.insert("local".into(), h1);
    app.hosts.insert("remote_box".into(), h2);

    router.update_from_app(&app);

    // 1. 送給遠端 session 的 Prompt 應路由至 remote_box
    let prompt_req = ertangle_protocol::Request::Prompt {
        session: "ses_rem".into(),
        text: "hello remote".into(),
        attachments: Vec::new(),
        queue: false,
    };
    router.send(&prompt_req).unwrap();

    let mut bytes = [0u8; 1024];
    let n = remote_r.read(&mut bytes).unwrap();
    let remote_msg = String::from_utf8_lossy(&bytes[..n]);
    assert!(
        remote_msg.contains("prompt"),
        "遠端主機應收到 prompt: {remote_msg}"
    );
    assert!(
        remote_msg.contains("ses_rem"),
        "遠端主機應收到 ses_rem: {remote_msg}"
    );
    assert!(
        remote_msg.contains("hello remote"),
        "遠端主機應收到 text: {remote_msg}"
    );

    // 2. 送給本機 session 的 Kill 應路由至 local
    let kill_req = ertangle_protocol::Request::Kill {
        session: "ses_loc".into(),
    };
    router.send(&kill_req).unwrap();

    let mut loc_bytes = [0u8; 1024];
    let n_loc = local_r.read(&mut loc_bytes).unwrap();
    let loc_msg = String::from_utf8_lossy(&loc_bytes[..n_loc]);
    assert!(loc_msg.contains("kill"), "本機主機應收到 kill: {loc_msg}");
    assert!(
        loc_msg.contains("ses_loc"),
        "本機主機應收到 ses_loc: {loc_msg}"
    );
}

#[test]
fn markdown_links_are_clickable_by_label_without_raw_url() {
    let app = sample_app(&["check [index.html](file:///path/to/index.html) now"]);
    let id: SessionId = "ses_test".into();
    let (lines, links) = build_transcript_lines(&app, &id, 80);
    let text = lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert_eq!(text, "check index.html now", "不應把冗長 URL 塞入顯示文字");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].url, "file:///path/to/index.html");
    assert_eq!(links[0].col_start, 6);
    assert_eq!(links[0].col_end, 16);
}

#[test]
fn submit_spawn_parses_role_flag_coordinator_and_short_flag() {
    use std::io::BufRead;
    let mut app = test_app();
    let (mut w, peer) = pair_writer();
    let mut reader = std::io::BufReader::new(peer);

    // 1. --role coordinator
    crate::commands::submit_spawn(
        &mut app,
        &mut w,
        "--role coordinator claude:opus plan the work",
    )
    .unwrap();
    let mut line1 = String::new();
    reader.read_line(&mut line1).unwrap();
    let req1: Request = ertangle_protocol::decode_line(&line1).unwrap();
    match req1 {
        Request::Spawn(spawn) => {
            assert_eq!(spawn.role, Some(Role::Coordinator));
            assert_eq!(spawn.kind, Some(AgentKind::ClaudeCode));
            assert_eq!(spawn.model.as_deref(), Some("opus"));
            assert_eq!(spawn.prompt.as_deref(), Some("plan the work"));
        }
        other => panic!("unexpected req: {other:?}"),
    }

    // 2. -r reviewer
    crate::commands::submit_spawn(&mut app, &mut w, "-r reviewer opencode review the diff")
        .unwrap();
    let mut line2 = String::new();
    reader.read_line(&mut line2).unwrap();
    let req2: Request = ertangle_protocol::decode_line(&line2).unwrap();
    match req2 {
        Request::Spawn(spawn) => {
            assert_eq!(spawn.role, Some(Role::Reviewer));
            assert_eq!(spawn.kind, Some(AgentKind::OpenCode));
            assert_eq!(spawn.prompt.as_deref(), Some("review the diff"));
        }
        other => panic!("unexpected req: {other:?}"),
    }
}

#[test]
fn spawn_mode_ctrl_r_cycles_role() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let (mut w, _) = pair_writer();
    app.mode = Mode::Spawn;
    app.spawn_role = Role::Worker;

    // 按 Ctrl-R: Worker -> Coordinator
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    crate::input::handle_key(&mut app, &mut w, ctrl_r).unwrap();
    assert_eq!(app.spawn_role, Role::Coordinator);
    assert!(app.status.contains("coordinator"));

    // 按 Ctrl-R: Coordinator -> Reviewer
    crate::input::handle_key(&mut app, &mut w, ctrl_r).unwrap();
    assert_eq!(app.spawn_role, Role::Reviewer);
    assert!(app.status.contains("reviewer"));

    // 按 Ctrl-R: Reviewer -> Worker
    crate::input::handle_key(&mut app, &mut w, ctrl_r).unwrap();
    assert_eq!(app.spawn_role, Role::Worker);
    assert!(app.status.contains("worker"));
}

#[test]
fn visible_transcript_with_active_tool_and_subagent() {
    use ertangle_core::ActiveTool;
    let now = ertangle_core::now_millis();

    let src: Vec<String> = vec![
        "start".into(),
        "▸ Bash echo 1".into(),
        "  ✓ 1".into(),
        "▸ Bash echo 2".into(),
        "  ✓ 2".into(),
        "▸ Bash sleep 60".into(),
    ];

    let active = vec![ActiveTool {
        name: "Bash".into(),
        summary: "sleep 60".into(),
        started_ms: now.saturating_sub(12_000),
        is_subagent: false,
    }];

    // 摺疊模式下：前兩次成功的呼叫摺為一行，進行中的 sleep 60 獨立顯示在最下方
    let folded = crate::app::visible_transcript(src.iter(), false, false, &active);
    assert_eq!(folded.len(), 3, "{folded:?}");
    assert_eq!(folded[0], "start");
    assert!(folded[1].contains("工具 ×2 (✓2)"), "{folded:?}");
    assert!(
        folded[2].contains("▸ [執行中] Bash: sleep 60 (12s)"),
        "{folded:?}"
    );

    // 原生 Subagent 測試
    let sub_active = vec![ActiveTool {
        name: "Task".into(),
        summary: "audit codebase".into(),
        started_ms: now.saturating_sub(5_000),
        is_subagent: true,
    }];
    let sub_src: Vec<String> = vec!["start".into(), "▸ Task audit codebase".into()];
    let sub_folded = crate::app::visible_transcript(sub_src.iter(), false, false, &sub_active);
    assert_eq!(sub_folded.len(), 2, "{sub_folded:?}");
    assert!(
        sub_folded[1].contains("▸ [subagent] Task: audit codebase (5s)"),
        "{sub_folded:?}"
    );
}

#[test]
fn ps_modal_navigation_and_kill() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ertangle_core::BgProc;
    use ertangle_protocol::Request;
    use std::io::BufRead;

    let mut app = sample_app(&[]);
    let (mut w, peer) = pair_writer();
    let mut reader = std::io::BufReader::new(peer);

    // 為 ses_test 設定背景處理程序
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.bg_procs = vec![
            BgProc {
                pid: 1001,
                name: "node".into(),
                cmdline: "node server.js".into(),
                elapsed_secs: 25,
            },
            BgProc {
                pid: 1002,
                name: "vite".into(),
                cmdline: "vite --port 5173".into(),
                elapsed_secs: 10,
            },
        ];
    }

    // 透過 :ps 開啟浮層
    crate::commands::execute_command(&mut app, &mut w, "ps").unwrap();
    assert_eq!(app.mode, Mode::Ps);
    assert!(app.ps_modal.is_some());
    assert_eq!(app.ps_modal.as_ref().unwrap().cursor, 0);

    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);

    // 下移游標選取第 2 筆 (PID 1002)
    crate::input::handle_key(&mut app, &mut w, key(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.ps_modal.as_ref().unwrap().cursor, 1);

    // 按 x 單獨終止 PID 1002 (第 1 次：SIGTERM，force = false)
    crate::input::handle_key(&mut app, &mut w, key(KeyCode::Char('x'))).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let req1: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    assert!(matches!(
        req1,
        Request::KillBgProc {
            ref session,
            pid: Some(1002),
            force: false,
        } if session == "ses_test"
    ));
    assert!(app.status.contains("SIGTERM"));

    // 對同一個仍在跑的 PID 再按一次 x (第 2 次：SIGKILL，force = true)
    crate::input::handle_key(&mut app, &mut w, key(KeyCode::Char('x'))).unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let req2: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    assert!(matches!(
        req2,
        Request::KillBgProc {
            ref session,
            pid: Some(1002),
            force: true,
        } if session == "ses_test"
    ));
    assert!(app.status.contains("SIGKILL"));

    // 按 X 清理全部背景處理程序
    let shift_x = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT);
    crate::input::handle_key(&mut app, &mut w, shift_x).unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let req3: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    assert!(matches!(
        req3,
        Request::KillBgProc {
            ref session,
            pid: None,
            force: false,
        } if session == "ses_test"
    ));

    // 按 Esc 關閉浮層回到 Normal
    crate::input::handle_key(&mut app, &mut w, key(KeyCode::Esc)).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.ps_modal.is_none());
}

#[test]
fn bg_kill_command_syntax() {
    use ertangle_protocol::Request;
    use std::io::BufRead;

    let mut app = sample_app(&[]);
    let (mut w, peer) = pair_writer();
    let mut reader = std::io::BufReader::new(peer);

    // 未帶參數應提示用法
    crate::commands::execute_command(&mut app, &mut w, "bg-kill").unwrap();
    assert!(app.status.contains("用法"));

    // 指定 PID
    crate::commands::execute_command(&mut app, &mut w, "bg-kill 48291").unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let req1: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    assert!(matches!(
        req1,
        Request::KillBgProc {
            ref session,
            pid: Some(48291),
            force: false,
        } if session == "ses_test"
    ));

    // 指定 --all
    crate::commands::execute_command(&mut app, &mut w, "bg-kill --all").unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let req2: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    assert!(matches!(
        req2,
        Request::KillBgProc {
            ref session,
            pid: None,
            force: false,
        } if session == "ses_test"
    ));
}

#[test]
fn session_list_and_footer_active_and_bg_indicators() {
    let mut app = sample_app(&[]);
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Busy;
        s.active_tools = vec![ertangle_core::ActiveTool {
            name: "Bash".into(),
            summary: "sleep 60".into(),
            started_ms: ertangle_core::now_millis(),
            is_subagent: false,
        }];
        s.bg_procs = vec![ertangle_core::BgProc {
            pid: 1234,
            name: "node".into(),
            cmdline: "node server.js".into(),
            elapsed_secs: 5,
        }];
    }
    let text = render(&mut app);
    // 側欄只留短標記 (bg1)，完整內容在 footer 與逐字稿裡；工具呼叫也只出現在逐字稿。
    assert!(text.contains("bg1"), "{text}");
    assert!(text.contains("bg:1 (node)"), "{text}");
    assert!(text.contains("Bash: sleep 60"), "{text}");
    assert!(!text.contains("run:Bash"), "側欄不該再塞工具名稱：{text}");

    // 回到 Idle，bg 標記仍保留 (黃色提醒) 且 footer 仍有完整名稱。
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Idle;
        s.active_tools.clear();
    }
    let text_idle = render(&mut app);
    assert!(text_idle.contains("bg1"), "{text_idle}");
    assert!(text_idle.contains("bg:1 (node)"), "{text_idle}");
}

#[test]
fn mouse_click_on_footer_opens_ps_modal() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.bg_procs = vec![ertangle_core::BgProc {
            pid: 1234,
            name: "node".into(),
            cmdline: "node".into(),
            elapsed_secs: 1,
        }];
    }
    render_size(&mut app, 120, 40);
    assert_eq!(app.mode, Mode::Normal);

    // 點擊逐字稿底部邊框 (Footer)
    let footer_row = app.transcript_area.bottom().saturating_sub(1);
    let footer_col = app.transcript_area.x + 5;
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: footer_col,
        row: footer_row,
        modifiers: KeyModifiers::NONE,
    };
    crate::mouse::handle_mouse(&mut app, &mut w, ev).unwrap();
    assert_eq!(app.mode, Mode::Ps);
    assert!(app.ps_modal.is_some());
}

#[test]
fn diff_file_jump_and_collapse_preserves_gutter() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.view = CenterView::Diff;
    let raw_diff = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 fn a() {
+    println!(\"a\");
 }
diff --git a/src/b.rs b/src/b.rs
index 3333333..4444444 100644
--- a/src/b.rs
+++ b/src/b.rs
@@ -10,3 +10,4 @@
 fn b() {
-    old_b();
+    new_b1();
+    new_b2();
 }
diff --git a/src/c.rs b/src/c.rs
index 5555555..6666666 100644
--- a/src/c.rs
+++ b/src/c.rs
@@ -20,2 +20,3 @@
 fn c() {
+    c_added();
 }";
    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Diff {
            session: "ses_test".into(),
            branch: Some("ertangle/ses_test".into()),
            cwd: "/tmp".into(),
            diff: raw_diff.into(),
        },
    );

    assert_eq!(app.diff_files.len(), 3);
    assert_eq!(app.diff_files[0].path, "src/a.rs");
    assert_eq!(app.diff_files[1].path, "src/b.rs");
    assert_eq!(app.diff_files[1].added, 2);
    assert_eq!(app.diff_files[1].removed, 1);
    assert_eq!(app.diff_files[2].path, "src/c.rs");

    // 初始在檔案 1 的標頭 (行 0)
    assert_eq!(app.diff_cursor, 0);
    let rendered = render(&mut app);
    assert!(rendered.contains("[1/3] src/a.rs"), "{rendered}");

    // 按 `)` 跳至第 2 個檔案標頭
    handle_key(&mut app, &mut w, kev(KeyCode::Char(')'))).unwrap();
    assert_eq!(app.diff_cursor, app.diff_files[1].start_line);
    assert!(app.diff_lines[app.diff_cursor].starts_with("diff --git a/src/b.rs"));
    let rendered = render(&mut app);
    assert!(rendered.contains("[2/3] src/b.rs"), "{rendered}");

    // 再按 `)` 跳至第 3 個檔案標頭
    handle_key(&mut app, &mut w, kev(KeyCode::Char(')'))).unwrap();
    assert_eq!(app.diff_cursor, app.diff_files[2].start_line);
    assert!(app.diff_lines[app.diff_cursor].starts_with("diff --git a/src/c.rs"));

    // 按 `(` 跳回第 2 個檔案標頭
    handle_key(&mut app, &mut w, kev(KeyCode::Char('('))).unwrap();
    assert_eq!(app.diff_cursor, app.diff_files[1].start_line);

    // 按 Enter 摺疊第 2 個檔案
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert!(app.diff_collapsed.contains("src/b.rs"));
    let b_line_idx = app.diff_cursor;
    assert!(
        app.diff_lines[b_line_idx].contains("src/b.rs (+2 -1)"),
        "第 2 個檔案應濃縮為單行摘要：{}",
        app.diff_lines[b_line_idx]
    );
    assert_eq!(app.diff_gutter[b_line_idx], (None, None));

    // 第 3 個檔案緊接在摺疊行之後，且行號邊欄 (20, 21, 22) 依然正確
    let c_start = b_line_idx + 1;
    assert_eq!(app.diff_lines[c_start], "diff --git a/src/c.rs b/src/c.rs");
    // c_start + 4 是 `@@ -20,2 +20,3 @@`
    assert_eq!(app.diff_gutter[c_start + 4], (None, None));
    // c_start + 5 是 ` fn c() {` -> (Some(20), Some(20))
    assert_eq!(app.diff_gutter[c_start + 5], (Some(20), Some(20)));
    // c_start + 6 是 `+    c_added();` -> (None, Some(21))
    assert_eq!(app.diff_gutter[c_start + 6], (None, Some(21)));
    // c_start + 7 是 ` }` -> (Some(21), Some(22))
    assert_eq!(app.diff_gutter[c_start + 7], (Some(21), Some(22)));
}

#[test]
fn diff_extract_hunk_patch_and_discard_confirmation() {
    let mut app = sample_app(&[]);
    let (mut w, peer) = pair_writer();
    app.view = CenterView::Diff;
    let raw_diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 line1
-line2
+line2_mod
 line3
@@ -10,3 +10,3 @@
 line10
-line11
+line11_mod
 line12
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,2 @@
+hello
+world";
    app.apply_host_frame(
        "local",
        ertangle_protocol::Frame::Diff {
            session: "ses_test".into(),
            branch: Some("ertangle/ses_test".into()),
            cwd: "/tmp".into(),
            diff: raw_diff.into(),
        },
    );

    // 游標在檔案標頭行 (行 0) -> 整檔退回 (patch 為 None)
    app.diff_cursor = 0;
    let (file, patch) = crate::diff::extract_current_hunk_patch(&app).unwrap();
    assert_eq!(file, "src/lib.rs");
    assert!(patch.is_none());

    // 游標在第 1 個 hunk 內 (行 6: `-line2`) -> 單一 hunk 退回
    app.diff_cursor = 6;
    let (file, patch) = crate::diff::extract_current_hunk_patch(&app).unwrap();
    assert_eq!(file, "src/lib.rs");
    let patch_str = patch.expect("應產生單一 hunk patch");
    assert!(patch_str.contains("@@ -1,3 +1,3 @@"));
    assert!(patch_str.contains("-line2\n+line2_mod"));
    assert!(!patch_str.contains("@@ -10,3 +10,3 @@"));

    // 未追蹤新檔 -> 強制整檔退回 (patch 為 None)
    app.diff_cursor = app.diff_lines.len() - 1;
    let (file_untracked, patch_untracked) = crate::diff::extract_current_hunk_patch(&app).unwrap();
    assert_eq!(file_untracked, "src/new.rs");
    assert!(patch_untracked.is_none());

    // 移回第 1 個 hunk 按 X：第一次僅提示確認，第二次才送出 Request::WorktreeDiscard
    app.diff_cursor = 6;
    handle_key(&mut app, &mut w, kev(KeyCode::Char('X'))).unwrap();
    assert!(app.diff_discard_armed.is_some());
    handle_key(&mut app, &mut w, kev(KeyCode::Char('X'))).unwrap();
    assert!(app.diff_discard_armed.is_none());

    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(peer);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let req: Request = ertangle_protocol::decode_line(line.trim()).unwrap();
    match req {
        Request::WorktreeDiscard {
            session,
            file,
            patch,
        } => {
            assert_eq!(session, "ses_test");
            assert_eq!(file, "src/lib.rs");
            assert!(patch.unwrap().contains("@@ -1,3 +1,3 @@"));
        }
        other => panic!("預期 WorktreeDiscard，收到 {other:?}"),
    }
}

fn task(
    id: &str,
    title: &str,
    status: ertangle_core::TaskStatus,
    who: &str,
) -> ertangle_core::Task {
    ertangle_core::Task {
        id: id.into(),
        title: title.into(),
        detail: String::new(),
        status,
        assignee: Some(who.into()),
        created_by: None,
        depends_on: Vec::new(),
        updated_ms: 1,
    }
}

fn lines_text(lines: &[ratatui::text::Line]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn project_panel_shows_collaboration_running_and_hides_empty() {
    let mut app = sample_app(&[]);
    app.home_project = "/work/alpha".into();
    app.scope_here = true;
    app.state.sessions.get_mut("ses_test").unwrap().cwd = "/work/alpha".into();
    app.state.sessions.get_mut("ses_test").unwrap().status = SessionStatus::Idle;
    // 空狀態：只有提示，沒有任何區段標題。
    let txt = lines_text(&crate::project::project_panel_lines(&app, 100_000));
    assert_eq!(txt.lines().count(), 1, "{txt}");

    add_session(&mut app, "ses_web", "worker/web", 5);
    add_session(&mut app, "ses_other", "other", 6);
    {
        let w = app.state.sessions.get_mut("ses_web").unwrap();
        w.cwd = "/work/alpha".into();
        w.parent = Some("ses_test".into());
        w.status_note = Some("編輯中".into());
        w.active_tools.push(ertangle_core::ActiveTool {
            name: "Bash".into(),
            summary: "cargo test".into(),
            started_ms: 88_000,
            is_subagent: false,
        });
        w.active_tools.push(ertangle_core::ActiveTool {
            name: "Task".into(),
            summary: String::new(),
            started_ms: 90_000,
            is_subagent: true,
        });
        let o = app.state.sessions.get_mut("ses_other").unwrap();
        o.cwd = "/work/beta".into();
        o.bg_procs.push(ertangle_core::BgProc {
            pid: 999,
            name: "leak".into(),
            cmdline: String::new(),
            elapsed_secs: 1,
        });
    }
    let t = task(
        "t1",
        "Fix web scope",
        ertangle_core::TaskStatus::Doing,
        "ses_web",
    );
    app.state.tasks.insert(t.id.clone(), t);
    let t = task(
        "t2",
        "Done thing",
        ertangle_core::TaskStatus::Done,
        "ses_web",
    );
    app.state.tasks.insert(t.id.clone(), t);
    let txt = lines_text(&crate::project::project_panel_lines(&app, 100_000));
    assert!(
        txt.contains("協作") && txt.contains("↳ ○ worker/web"),
        "{txt}"
    );
    assert!(txt.contains("編輯中") && txt.contains("doing: Fix web scope"));
    assert!(!txt.contains("Done thing"));
    assert!(txt.contains("run:Bash") && txt.contains("12s") && txt.contains("sub:Task"));
    // 其他專案的 bg 處理程序不在範圍內。
    assert!(!txt.contains("leak"));
    app.scope_here = false;
    let txt = lines_text(&crate::project::project_panel_lines(&app, 100_000));
    assert!(txt.contains("bg:999"));
}

#[test]
fn grid_paging_and_rects() {
    use crate::project::{grid_page, grid_rects};
    assert_eq!(grid_page(0, 0), (0, 0, 1));
    assert_eq!(grid_page(2, 3), (0, 3, 1));
    assert_eq!(grid_page(3, 6), (0, 4, 2));
    assert_eq!(grid_page(4, 6), (4, 2, 2));
    assert_eq!(grid_page(9, 6), (4, 2, 2));
    let a = Rect::new(0, 0, 120, 40);
    assert_eq!(grid_rects(a, 1), vec![a]);
    let two = grid_rects(a, 2);
    assert_eq!(two[0].height, 40);
    let narrow = grid_rects(Rect::new(0, 0, 80, 40), 2);
    assert_eq!(narrow[0].width, 80);
    assert_eq!(grid_rects(a, 3)[2].width, 120);
    assert_eq!(grid_rects(a, 4).len(), 4);
}

#[test]
fn grid_view_renders_page_with_selected_and_toggles() {
    let mut app = sample_app(&["hello-main"]);
    for i in 0..5 {
        let id = format!("ses_g{i}");
        add_session(&mut app, &id, &format!("grid{i}"), 10 + i);
        app.state
            .transcripts
            .insert(id, VecDeque::from(vec![format!("body-g{i}")]));
    }
    app.scope_here = false;
    let (mut w, _peer) = pair_writer();
    execute_command(&mut app, &mut w, "grid").unwrap();
    assert_eq!(app.view, CenterView::Grid);
    let text = render(&mut app);
    // 6 個 session：第一頁 4 個 (含 sample 與 grid0..2)，第二頁不顯示。
    assert!(
        text.contains("body-g0") && !text.contains("body-g4"),
        "{text}"
    );
    assert!(text.contains("1/2"));
    app.selected = 5;
    let text = render(&mut app);
    assert!(text.contains("body-g4") && text.contains("2/2"));
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.view, CenterView::Transcript);
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.view, CenterView::Grid);
}

#[test]
fn skill_command_installs_shows_and_uninstalls_in_project() {
    let t = std::env::temp_dir().join(ertangle_core::new_id("tuiskill"));
    std::fs::create_dir_all(&t).unwrap();
    let show = crate::commands::skill_action(&t, "show", "claude").unwrap();
    assert!(
        show.contains("未安裝") || show.contains("not installed"),
        "{show}"
    );
    crate::commands::skill_action(&t, "install", "claude").unwrap();
    assert!(t.join(".claude/skills/ertangle-ade/SKILL.md").is_file());
    crate::commands::skill_action(&t, "install", "agents").unwrap();
    assert!(t.join(".agents/skills/ertangle-ade/SKILL.md").is_file());
    crate::commands::skill_action(&t, "uninstall", "claude").unwrap();
    assert!(!t.join(".claude/skills/ertangle-ade").exists());
    assert!(crate::commands::skill_action(&t, "bogus", "claude").is_err());
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
fn inject_skill_is_a_settings_toggle() {
    use crate::settings::{Item, ITEMS};
    assert!(ITEMS.contains(&Item::InjectSkill));
    assert!(Item::InjectSkill.is_toggle());
}

#[test]
fn token_hints_is_a_settings_toggle() {
    use crate::settings::{Item, ITEMS};
    assert!(ITEMS.contains(&Item::TokenHints));
    assert!(Item::TokenHints.is_toggle());
}

#[test]
fn quota_bar_fills_proportionally_and_clamps() {
    let (f, e) = crate::quota::bar(56.0, 20);
    assert_eq!((f.chars().count(), e.chars().count()), (11, 9));
    assert_eq!(crate::quota::bar(150.0, 10).0.chars().count(), 10);
    assert_eq!(crate::quota::bar(-5.0, 10).1.chars().count(), 10);
}

#[test]
fn quota_popup_lines_show_bars_resets_and_missing_data() {
    use ertangle_core::{Quota, QuotaWindow};
    let now = 10 * 3_600_000;
    let q = Quota {
        agent: "claude".into(),
        windows: vec![
            QuotaWindow {
                name: "5h".into(),
                used_percent: 56.0,
                resets_at_ms: Some(now + 2 * 3_600_000),
            },
            QuotaWindow {
                name: "7d".into(),
                used_percent: 80.0,
                resets_at_ms: Some(now - 1),
            },
        ],
        reached: false,
        warning: false,
        updated_ms: now,
    };
    let text = |lines: Vec<ratatui::text::Line<'static>>| {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let l = text(crate::quota::agent_lines("claude", Some(&q), now));
    assert_eq!(l.len(), 3, "{l:?}");
    assert!(
        l[1].contains("5h") && l[1].contains("█") && l[1].contains("44%"),
        "{l:?}"
    );
    // 已過重置時間的視窗恢復為 100% 剩餘額度。
    assert!(l[2].contains("7d") && l[2].contains("100%"), "{l:?}");
    let mut w = q.clone();
    w.warning = true;
    assert!(text(crate::quota::agent_lines("claude", Some(&w), now))[0].contains("⚠"));
    let none = text(crate::quota::agent_lines("opencode", None, now));
    assert_eq!(none.len(), 1);
    assert!(none[0].starts_with("opencode"));
}

#[test]
fn quota_popup_renders_and_header_click_area_is_set() {
    use ertangle_core::{Quota, QuotaWindow};
    let mut app = sample_app(&[]);
    let now = ertangle_core::now_millis();
    app.state.quotas.insert(
        "claude".into(),
        Quota {
            agent: "claude".into(),
            windows: vec![QuotaWindow {
                name: "5h".into(),
                used_percent: 42.0,
                resets_at_ms: Some(now + 3_600_000),
            }],
            reached: false,
            warning: false,
            updated_ms: now,
        },
    );
    app.quota_agents = vec!["claude".into(), "codex".into()];
    app.mode = Mode::Quota;
    let out = render(&mut app);
    assert!(
        out.contains("claude") && out.contains("codex") && out.contains("58%"),
        "{out}"
    );
    assert!(app.quota_area.width > 0, "標題列額度摘要應可點擊");

    // 切換到第 1 個 agent 頁籤 (claude)，只顯示 claude。
    crate::quota::handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.quota_tab, 1);
    let out_tab1 = render(&mut app);
    assert!(out_tab1.contains("58%"), "{out_tab1}");

    // 驗證 antigravity 視窗名稱轉為易讀標籤。
    let agy_q = Quota {
        agent: "antigravity".into(),
        windows: vec![QuotaWindow {
            name: "3p-5h".into(),
            used_percent: 10.0,
            resets_at_ms: Some(now + 3_600_000),
        }],
        reached: false,
        warning: false,
        updated_ms: now,
    };
    let agy_lines = crate::quota::agent_lines("antigravity", Some(&agy_q), now);
    let agy_text: String = agy_lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(agy_text.contains("Claude / GPT (5h)"), "{agy_text}");

    // `quota::open` 應將有回報額度的 agent 排在前面，沒有資料的 (如 opencode) 排在後面。
    crate::quota::open(&mut app);
    if let (Some(cl_pos), Some(oc_pos)) = (
        app.quota_agents.iter().position(|a| a == "claude"),
        app.quota_agents.iter().position(|a| a == "opencode"),
    ) {
        assert!(cl_pos < oc_pos, "{:?}", app.quota_agents);
    }
}

fn generate_realistic_transcript(target_lines: usize) -> Vec<String> {
    let template = [
        "❯ Please implement the feature and check tests",
        "💭 Analyzing requirements and scanning existing code...",
        "## Implementation Plan",
        "Here is the plan to optimize the rendering path with **bold text**, *italic text*, `inline_code()`, and a [link](https://example.com/repo).",
        "We need to consider the following aspects:",
        "- 1. Check current bottlenecks using profiling or benchmarks.",
        "- 2. Avoid intermediate allocations of `Vec<String>` where possible.",
        "- 3. Keep an incremental prefix cache for rendered lines.",
        "```rust",
        "pub struct Cache {",
        "    pub prefix_lines: Vec<Line<'static>>,",
        "    pub links: Vec<LinkTarget>,",
        "    pub valid_len: usize,",
        "}",
        "```",
        "Now running tool invocations to inspect files:",
        "▸ read_file: path=\"crates/ertangle-tui/src/draw.rs\"",
        "  ✓ read 120 lines",
        "▸ run_command: cargo check --workspace",
        "  ✓ finished with exit code 0",
        "▸ edit_file: path=\"crates/ertangle-tui/src/markdown.rs\"",
        "  ✓ successfully updated 2 hunks",
        "Performance comparison table:",
        "| Step | Target | Expected |",
        "|---|---|---|",
        "| Cold Rebuild | 4000 lines | fast |",
        "| Incremental | 1 line append | ultra fast |",
        "",
        "> Quote from design docs: correctness first, conservative safe boundary.",
        "End of cycle section.",
    ];
    let mut out = Vec::with_capacity(target_lines);
    while out.len() < target_lines {
        for line in &template {
            if out.len() >= target_lines {
                break;
            }
            out.push(line.to_string());
        }
    }
    out
}

#[test]
fn transcript_perf_report() {
    let lines = generate_realistic_transcript(4000);
    assert_eq!(lines.len(), 4000);

    let mut app = sample_app(&[]);
    let deque: VecDeque<String> = lines.clone().into();
    app.state.transcripts.insert("ses_test".into(), deque);

    let id = "ses_test".to_string();
    let width = 100;

    // Warm up
    let _ = crate::app::visible_transcript(lines.iter(), app.show_thoughts, app.show_tools, &[]);
    let _ = crate::draw::build_transcript_lines(&app, &id, width);

    let runs = 11;
    let mut vis_durations = Vec::new();
    let mut build_durations = Vec::new();
    let mut inc_durations = Vec::new();

    for _ in 0..runs {
        let t0 = std::time::Instant::now();
        let vis =
            crate::app::visible_transcript(lines.iter(), app.show_thoughts, app.show_tools, &[]);
        let elapsed_vis = t0.elapsed();
        vis_durations.push(elapsed_vis);
        std::hint::black_box(&vis);

        let t1 = std::time::Instant::now();
        let (built_lines, built_links) = crate::draw::build_transcript_lines(&app, &id, width);
        let elapsed_build = t1.elapsed();
        build_durations.push(elapsed_build);
        std::hint::black_box((&built_lines, &built_links));
    }

    // Measure append-only incremental rebuild
    crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    for _ in 0..runs {
        app.state
            .transcripts
            .get_mut(&id)
            .unwrap()
            .push_back("incremental test append line".to_string());
        let t2 = std::time::Instant::now();
        crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
        let elapsed_inc = t2.elapsed();
        inc_durations.push(elapsed_inc);
        // Pop the line back and restore cache for next iteration
        app.state.transcripts.get_mut(&id).unwrap().pop_back();
        crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    }

    vis_durations.sort();
    build_durations.sort();
    inc_durations.sort();

    let median_vis = vis_durations[runs / 2];
    let median_build = build_durations[runs / 2];
    let median_inc = inc_durations[runs / 2];

    println!("\n=== TRANSCRIPT PERF REPORT (AFTER) ===");
    println!("Lines: {}", lines.len());
    println!("Runs: {}", runs);
    println!(
        "visible_transcript: median = {:?} (min: {:?}, max: {:?})",
        median_vis,
        vis_durations[0],
        vis_durations[runs - 1]
    );
    println!(
        "build_transcript_lines (cold rebuild, width {}): median = {:?} (min: {:?}, max: {:?})",
        width,
        median_build,
        build_durations[0],
        build_durations[runs - 1]
    );
    println!(
        "update_transcript_cache (append 1 line incremental): median = {:?} (min: {:?}, max: {:?})",
        median_inc,
        inc_durations[0],
        inc_durations[runs - 1]
    );
    println!("======================================\n");
}

fn transcript_lines_to_plain(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn append_only_incremental_rebuild_matches_cold_rebuild() {
    let lines = generate_realistic_transcript(1000);
    let mut app = sample_app(&[]);
    let deque: VecDeque<String> = lines.into();
    app.state.transcripts.insert("ses_test".into(), deque);
    let id = "ses_test".to_string();
    let width = 90;

    // Build initial cache
    crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    assert!(app.md_cache.is_some());

    // Append several new lines (mix of markdown, link, tool)
    let new_lines = [
        "### Newly Appended Heading",
        "Here is new text with [new link](https://example.com/new).",
        "▸ read_file: path=\"new_file.rs\"",
        "  ✓ read 50 lines",
        "Final trailing note.",
    ];
    for nl in &new_lines {
        app.state
            .transcripts
            .get_mut(&id)
            .unwrap()
            .push_back(nl.to_string());
    }

    // Incremental rebuild
    crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    let cache = app.md_cache.as_ref().unwrap();

    // Cold full rebuild
    let (cold_lines, cold_links) = crate::draw::build_transcript_lines(&app, &id, width);

    // Compare lines
    let inc_text = transcript_lines_to_plain(&cache.lines);
    let cold_text = transcript_lines_to_plain(&cold_lines);
    assert_eq!(inc_text, cold_text, "Rendered line text must match exactly");

    // Compare links
    assert_eq!(
        cache.links, cold_links,
        "Extracted links and line indexes must match exactly"
    );
}

#[test]
fn streaming_line_changed_in_place_same_length_invalidates_and_updates() {
    let mut app = sample_app(&["first line", "", "initial line content ABC"]);
    let id = "ses_test".to_string();
    let width = 80;

    crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    let initial_stamp = app.md_cache.as_ref().unwrap().stamp;
    let initial_text = transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines);
    assert!(initial_text
        .iter()
        .any(|l| l.contains("initial line content ABC")));

    // Change last line in place with EXACT same length
    let last = app
        .state
        .transcripts
        .get_mut(&id)
        .unwrap()
        .back_mut()
        .unwrap();
    *last = "initial line content XYZ".to_string(); // ABC -> XYZ, same length!

    let new_stamp = crate::draw::transcript_stamp(&app, &id);
    assert_ne!(
        initial_stamp, new_stamp,
        "Stamp must change even when length is identical"
    );

    // Rebuild cache
    crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    let updated_text = transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines);
    assert!(
        !updated_text.iter().any(|l| l.contains("ABC")),
        "Old text must be gone"
    );
    assert!(
        updated_text.iter().any(|l| l.contains("XYZ")),
        "New in-place text must appear"
    );
}

#[test]
fn rebuild_on_width_change_session_switch_and_toggles_match_cold_rebuild() {
    let lines = generate_realistic_transcript(200);
    let mut app = sample_app(&[]);
    let deque: VecDeque<String> = lines.into();
    app.state.transcripts.insert("ses_test".into(), deque);
    let id = "ses_test".to_string();

    // 1. Initial build at width 100
    crate::draw::update_transcript_cache(&mut app, &id, 100, 100);

    // 2. Width change to 60
    crate::draw::update_transcript_cache(&mut app, &id, 60, 60);
    let (cold_lines_w, cold_links_w) = crate::draw::build_transcript_lines(&app, &id, 60);
    assert_eq!(
        transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines),
        transcript_lines_to_plain(&cold_lines_w)
    );
    assert_eq!(app.md_cache.as_ref().unwrap().links, cold_links_w);

    // 3. Raw mode toggle
    app.raw_md = true;
    crate::draw::update_transcript_cache(&mut app, &id, 60, 60);
    let (cold_lines_raw, _) = crate::draw::build_transcript_lines(&app, &id, 60);
    assert_eq!(
        transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines),
        transcript_lines_to_plain(&cold_lines_raw)
    );
    app.raw_md = false;

    // 4. show_tools toggle
    app.show_tools = true;
    crate::draw::update_transcript_cache(&mut app, &id, 60, 60);
    let (cold_lines_tools, _) = crate::draw::build_transcript_lines(&app, &id, 60);
    assert_eq!(
        transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines),
        transcript_lines_to_plain(&cold_lines_tools)
    );
    app.show_tools = false;

    // 5. show_thoughts toggle
    app.show_thoughts = true;
    crate::draw::update_transcript_cache(&mut app, &id, 60, 60);
    let (cold_lines_thoughts, _) = crate::draw::build_transcript_lines(&app, &id, 60);
    assert_eq!(
        transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines),
        transcript_lines_to_plain(&cold_lines_thoughts)
    );
    app.show_thoughts = false;

    // 6. Session switch
    let id2 = "ses_test2".to_string();
    let s2 = app.state.sessions.get("ses_test").unwrap().clone();
    app.state.sessions.insert(id2.clone(), s2);
    app.state.transcripts.insert(
        id2.clone(),
        VecDeque::from(vec!["hello from ses2".to_string()]),
    );
    crate::draw::update_transcript_cache(&mut app, &id2, 60, 60);
    let (cold_lines_s2, _) = crate::draw::build_transcript_lines(&app, &id2, 60);
    assert_eq!(
        transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines),
        transcript_lines_to_plain(&cold_lines_s2)
    );
}

#[test]
fn tool_folding_identical_for_show_tools_false() {
    let mut app = sample_app(&[
        "header text",
        "",
        "▸ read_file: a",
        "  ✓ 1",
        "▸ read_file: b",
        "  ✓ 2",
        "▸ run_cmd: cargo test",
        "  ✓ 3",
        "trailing prose",
    ]);
    let id = "ses_test".to_string();
    let width = 80;

    app.show_tools = false;
    crate::draw::update_transcript_cache(&mut app, &id, width, width as u16);
    let cache_text = transcript_lines_to_plain(&app.md_cache.as_ref().unwrap().lines);
    let (cold_lines, _) = crate::draw::build_transcript_lines(&app, &id, width);
    let cold_text = transcript_lines_to_plain(&cold_lines);

    assert_eq!(cache_text, cold_text);
    assert!(cache_text.iter().any(|l| l.contains("工具 ×3 (✓3)")));
}

#[test]
fn test_resolve_active_project_precedence() {
    let tmp = std::env::temp_dir();
    let dir_flag = tmp.join(ertangle_core::new_id("flag"));
    let dir_cfg = tmp.join(ertangle_core::new_id("cfg"));
    let dir_cwd = tmp.join(ertangle_core::new_id("cwd"));
    std::fs::create_dir_all(&dir_flag).unwrap();
    std::fs::create_dir_all(&dir_cfg).unwrap();
    std::fs::create_dir_all(&dir_cwd).unwrap();

    let canon_flag = dir_flag.canonicalize().unwrap();
    let canon_cfg = dir_cfg.canonicalize().unwrap();
    let canon_cwd = dir_cwd.canonicalize().unwrap();

    // 1. Flag wins over config and cwd
    let res1 = crate::app::resolve_active_project(
        Some(&dir_flag),
        Some(dir_cfg.to_str().unwrap()),
        &dir_cwd,
    );
    assert_eq!(res1, canon_flag);

    // 2. Invalid/missing flag falls back to config
    let bad_flag = tmp.join("nonexistent_flag_dir");
    let res2 = crate::app::resolve_active_project(
        Some(&bad_flag),
        Some(dir_cfg.to_str().unwrap()),
        &dir_cwd,
    );
    assert_eq!(res2, canon_cfg);

    // 3. No flag: config wins over cwd
    let res3 = crate::app::resolve_active_project(None, Some(dir_cfg.to_str().unwrap()), &dir_cwd);
    assert_eq!(res3, canon_cfg);

    // 4. No flag, invalid/missing config falls back to cwd
    let res4 = crate::app::resolve_active_project(None, Some("/nonexistent/cfg/path"), &dir_cwd);
    assert_eq!(res4, canon_cwd);

    // 5. No flag, no config uses cwd
    let res5 = crate::app::resolve_active_project(None, None, &dir_cwd);
    assert_eq!(res5, canon_cwd);

    let _ = std::fs::remove_dir_all(&dir_flag);
    let _ = std::fs::remove_dir_all(&dir_cfg);
    let _ = std::fs::remove_dir_all(&dir_cwd);
}

#[test]
fn test_app_set_active_project_state_transition() {
    let mut app = test_app();
    let tmp = std::env::temp_dir();
    let target = tmp.join(ertangle_core::new_id("proj_target"));
    std::fs::create_dir_all(&target).unwrap();
    let canon = target.canonicalize().unwrap();

    app.scope_here = false;
    app.selection = Some(((1, 0), (2, 5)));
    app.selected = 5;

    let res = app.set_active_project(&target);
    assert!(res.is_ok());
    assert_eq!(app.active_project, canon);
    assert_eq!(app.home_project, canon.to_string_lossy());
    assert!(
        app.scope_here,
        "set_active_project should set scope_here = true"
    );
    assert_eq!(app.selected, 0, "selected session should be reset to 0");
    assert!(app.selection.is_none(), "text selection should be reset");
    assert!(app.rebuild_pending, "rebuild_pending should be armed");

    // Invalid path returns Err and leaves state untouched
    let non_existent = tmp.join("non_existent_sub_path");
    let err = app.set_active_project(&non_existent);
    assert!(err.is_err());
    assert_eq!(
        app.active_project, canon,
        "active_project unchanged on error"
    );

    let _ = std::fs::remove_dir_all(&target);
}

#[test]
fn test_resume_picker_scope_filtering_and_parent_resolution() {
    let mut app = test_app();
    app.home_project = "/work/alpha".into();

    let now = ertangle_core::now_millis();

    // Live parent session in /work/alpha
    let s_parent_live = Session {
        id: "ses_parent_live".into(),
        kind: AgentKind::OpenCode,
        label: "coord/alpha".into(),
        cwd: "/work/alpha".into(),
        branch: None,
        role: Role::Coordinator,
        status: SessionStatus::Busy,
        model: Some("o3".into()),
        cost: Cost::default(),
        parent: None,
        created_ms: now.saturating_sub(100_000),
        transport: "acp".into(),
        status_note: None,
        repo_root: Some("/work/alpha".into()),
        context: None,
        native_id: Some("n_p_live".into()),
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };
    app.state
        .sessions
        .insert("ses_parent_live".into(), s_parent_live.clone());

    // Ended child session in /work/alpha with live parent
    let mut s_child_alpha = s_parent_live.clone();
    s_child_alpha.id = "ses_child_alpha".into();
    s_child_alpha.label = "worker/alpha-1".into();
    s_child_alpha.status = SessionStatus::Exited;
    s_child_alpha.parent = Some("ses_parent_live".into());
    s_child_alpha.native_id = Some("n_child_a".into());
    s_child_alpha.created_ms = now.saturating_sub(50_000);
    app.state
        .sessions
        .insert("ses_child_alpha".into(), s_child_alpha);

    // Ended session in /work/alpha with unknown parent id
    let mut s_unknown_parent = s_parent_live.clone();
    s_unknown_parent.id = "ses_unk".into();
    s_unknown_parent.label = "worker/alpha-unk".into();
    s_unknown_parent.status = SessionStatus::Exited;
    s_unknown_parent.parent = Some("ses_9876543210".into());
    s_unknown_parent.native_id = Some("n_unk".into());
    s_unknown_parent.created_ms = now.saturating_sub(40_000);
    app.state
        .sessions
        .insert("ses_unk".into(), s_unknown_parent);

    // Archived session in /work/beta (different project)
    let mut s_arch_beta = s_parent_live.clone();
    s_arch_beta.id = "ses_arch_beta".into();
    s_arch_beta.label = "worker/beta-arch".into();
    s_arch_beta.cwd = "/work/beta".into();
    s_arch_beta.repo_root = Some("/work/beta".into());
    s_arch_beta.status = SessionStatus::Idle;
    s_arch_beta.native_id = Some("n_beta".into());
    s_arch_beta.created_ms = now.saturating_sub(30_000);
    app.state
        .archived_sessions
        .insert("ses_arch_beta".into(), s_arch_beta);

    // 1. In ResumeScope::Here: only sessions in /work/alpha, no project prefix, parent annotation resolved
    let items_here = crate::menu::rebuild_resume_items(&app, crate::menu::ResumeScope::Here);
    let ids_here: Vec<_> = items_here.iter().map(|(_, id)| id.as_str()).collect();
    assert_eq!(ids_here, vec!["ses_unk", "ses_child_alpha"]);

    // Verify parent label for live parent: ↳ coord/alpha
    let child_row = items_here
        .iter()
        .find(|(_, id)| id == "ses_child_alpha")
        .unwrap();
    assert!(
        child_row.0.contains("↳ coord/alpha"),
        "Should resolve live parent label: {}",
        child_row.0
    );
    assert!(
        !child_row.0.starts_with('['),
        "Here scope should NOT prefix project name: {}",
        child_row.0
    );

    // Verify parent label for unknown parent: short id ↳ 987654
    let unk_row = items_here.iter().find(|(_, id)| id == "ses_unk").unwrap();
    assert!(
        unk_row.0.contains("↳ 987654"),
        "Should fallback to short parent id: {}",
        unk_row.0
    );

    // 2. In ResumeScope::All: includes /work/beta, has project prefixes
    let items_all = crate::menu::rebuild_resume_items(&app, crate::menu::ResumeScope::All);
    let ids_all: Vec<_> = items_all.iter().map(|(_, id)| id.as_str()).collect();
    assert_eq!(ids_all, vec!["ses_arch_beta", "ses_unk", "ses_child_alpha"]);

    let beta_row = items_all
        .iter()
        .find(|(_, id)| id == "ses_arch_beta")
        .unwrap();
    assert!(
        beta_row.0.starts_with("[beta]"),
        "All scope should prefix project name: {}",
        beta_row.0
    );
}

#[test]
fn test_resume_picker_tab_toggle_and_project_command() {
    let mut app = test_app();
    app.home_project = "/work/alpha".into();
    app.scope_here = true;

    let now = ertangle_core::now_millis();
    let s_ended = Session {
        id: "ses_ended".into(),
        kind: AgentKind::OpenCode,
        label: "worker/alpha".into(),
        cwd: "/work/alpha".into(),
        branch: None,
        role: Role::Worker,
        status: SessionStatus::Exited,
        model: None,
        cost: Cost::default(),
        parent: None,
        created_ms: now,
        transport: "acp".into(),
        status_note: None,
        repo_root: Some("/work/alpha".into()),
        context: None,
        native_id: Some("n_end".into()),
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };
    app.state.sessions.insert("ses_ended".into(), s_ended);

    // Open resume picker
    crate::menu::start_resume_picker(&mut app);
    assert_eq!(app.mode, Mode::Pick);
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.resume_scope, crate::menu::ResumeScope::Here);

    let (mut w, _peer) = pair_writer();

    // Tab key toggles scope to All
    let tab_key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    crate::menu::handle_pick_key(&mut app, &mut w, tab_key).unwrap();
    assert_eq!(
        app.picker.as_ref().unwrap().resume_scope,
        crate::menu::ResumeScope::All,
        "Tab should toggle resume scope to All"
    );

    // Tab key toggles back to Here
    crate::menu::handle_pick_key(&mut app, &mut w, tab_key).unwrap();
    assert_eq!(
        app.picker.as_ref().unwrap().resume_scope,
        crate::menu::ResumeScope::Here,
        "Tab should toggle resume scope back to Here"
    );

    // Esc closes picker
    let esc_key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    crate::menu::handle_pick_key(&mut app, &mut w, esc_key).unwrap();
    assert!(app.picker.is_none());
    assert_eq!(app.mode, Mode::Normal);

    // Test :project commands
    // 1. :project here / all / toggle
    crate::commands::execute_command(&mut app, &mut w, "project here").unwrap();
    assert!(app.scope_here);
    crate::commands::execute_command(&mut app, &mut w, "project all").unwrap();
    assert!(!app.scope_here);
    crate::commands::execute_command(&mut app, &mut w, "project").unwrap();
    assert!(app.scope_here);

    // 2. :project open <path>
    let tmp = std::env::temp_dir();
    let p_dir = tmp.join(ertangle_core::new_id("open_proj"));
    std::fs::create_dir_all(&p_dir).unwrap();
    let canon = p_dir.canonicalize().unwrap();

    crate::commands::execute_command(
        &mut app,
        &mut w,
        &format!("project open {}", p_dir.display()),
    )
    .unwrap();
    assert_eq!(app.active_project, canon);
    assert!(app.scope_here);

    // 3. :project open without path opens project picker
    crate::commands::execute_command(&mut app, &mut w, "project open").unwrap();
    assert!(matches!(
        app.picker.as_ref().map(|p| &p.stage),
        Some(crate::menu::PickStage::Project)
    ));

    let _ = std::fs::remove_dir_all(&p_dir);
}

#[test]
fn test_question_picker_single_and_multi_select() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let s = Session {
        id: "ses_q".into(),
        kind: AgentKind::OpenCode,
        label: "worker/q".into(),
        cwd: "/work".into(),
        branch: None,
        role: Role::Worker,
        status: SessionStatus::Busy,
        model: None,
        cost: Cost::default(),
        parent: None,
        created_ms: ertangle_core::now_millis(),
        transport: "acp".into(),
        status_note: None,
        repo_root: None,
        context: None,
        native_id: None,
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
    };
    app.state.sessions.insert("ses_q".into(), s);
    app.selected = 0;

    let q = ertangle_core::UserQuestion {
        id: "qst_1".into(),
        session: "ses_q".into(),
        question: "Select flavor".into(),
        header: Some("Flavor".into()),
        options: vec![
            ertangle_core::QuestionOption::plain("Vanilla"),
            ertangle_core::QuestionOption::plain("Chocolate"),
        ],
        multi_select: true,
        custom: true,
    };
    app.state
        .pending_questions
        .insert("qst_1".into(), q.clone());

    assert_eq!(app.selected_question().unwrap().id, "qst_1");

    crate::menu::start_question_picker(&mut app, &q);
    assert_eq!(app.mode, Mode::Pick);
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.items.len(), 2);

    // Toggle first option with Space
    let space_key = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
    crate::menu::handle_pick_key(&mut app, &mut w, space_key).unwrap();
    if let crate::menu::PickStage::Question { selected, .. } = &app.picker.as_ref().unwrap().stage {
        assert!(selected.contains("Vanilla"));
    } else {
        panic!("unexpected stage");
    }

    // Press Enter to submit
    let enter_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    crate::menu::handle_pick_key(&mut app, &mut w, enter_key).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.picker.is_none());
}
