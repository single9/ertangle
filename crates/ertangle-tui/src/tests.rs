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
        ended_ms: None,
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
    // Consecutive thoughts (new and legacy prefixes) fold into one summary line whose
    // preview is the last non-empty thought.
    let mut app = sample_app(&[
        "⋯ first hidden thought",
        "💭 second thought",
        "⋯ last thought preview",
        "answer",
        "done",
    ]);
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
    assert!(text.contains("思考 (3 行)"), "no fold summary: {text}");
    assert!(text.contains("last thought preview"), "no preview: {text}");
    assert!(
        !text.contains("first hidden thought"),
        "thought leaked: {text}"
    );
    assert!(!text.contains("second thought"), "thought leaked: {text}");
    assert!(!text.contains("💭"), "emoji leaked: {text}");
    assert!(text.contains("answer"));
    assert!(text.contains("done"));
    assert!(!main_pane_text(&app).contains("first hidden thought"));

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
    assert!(text.contains("first hidden thought"));
    assert!(text.contains("second thought"));
    assert!(text.contains("last thought preview"));
    assert!(!text.contains("思考 (3 行)"), "fold summary should be gone");
    assert!(main_pane_text(&app).contains("first hidden thought"));
}

#[test]
fn thought_prefix_recognizes_new_and_legacy_markers() {
    assert_eq!(crate::app::strip_thought_prefix("⋯ hello"), Some("hello"));
    assert_eq!(
        crate::app::strip_thought_prefix("💭 legacy"),
        Some("legacy")
    );
    assert_eq!(crate::app::strip_thought_prefix("plain"), None);
    assert!(crate::draw::SPINNER_FRAMES.contains(&crate::draw::spinner_frame()));
}

#[test]
fn safe_boundary_does_not_split_a_consecutive_thought_run() {
    let all_thoughts: VecDeque<String> = (0..12).map(|i| format!("⋯ thought {i}")).collect();
    assert_eq!(crate::draw::find_safe_boundary(&all_thoughts), 0);

    let mut mixed: VecDeque<String> = (0..20).map(|i| format!("line {i}")).collect();
    mixed.push_back("⋯ first".into());
    mixed.push_back("💭 last".into());
    // The boundary may fall before the run, but never between its two lines.
    let boundary = crate::draw::find_safe_boundary(&mixed);
    assert!(boundary <= 20, "unexpected boundary {boundary}");
    assert_ne!(boundary, 21);
}

#[test]
fn busy_session_shows_live_thinking_indicator() {
    let mut app = sample_app(&[]);
    let text = render(&mut app);
    assert!(
        squash(&text).contains("思考中"),
        "no live indicator: {text}"
    );
    assert!(
        crate::draw::SPINNER_FRAMES
            .iter()
            .any(|frame| text.contains(frame)),
        "no spinner frame: {text}"
    );

    // Scrolling away from the latest output hides the live row. Render once first so the
    // markdown cache exists; a cold cache reset would clear the scroll offset.
    let long: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
    let refs: Vec<&str> = long.iter().map(String::as_str).collect();
    let mut scrolled = sample_app(&refs);
    let _ = render(&mut scrolled);
    scrolled.transcript_scroll = Some(0);
    let text = render(&mut scrolled);
    assert!(
        !squash(&text).contains("思考中"),
        "live row should be hidden: {text}"
    );
}

#[test]
fn live_thinking_indicator_does_not_overwrite_last_line_when_full() {
    let mut lines: Vec<String> = (0..50).map(|i| format!("history line {i}")).collect();
    lines.push("❯ my sent prompt message".into());
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut app = sample_app(&refs);
    let text = render(&mut app);
    assert!(
        text.contains("my sent prompt message"),
        "user prompt was overwritten by thinking indicator: {text}"
    );
    assert!(
        squash(&text).contains("思考中"),
        "thinking indicator is missing: {text}"
    );
    let sq = squash(&text);
    let prompt_pos = sq
        .find("mysentpromptmessage")
        .expect("user prompt in squashed text");
    let thinking_pos = sq
        .find("思考中")
        .expect("thinking indicator in squashed text");
    assert!(
        prompt_pos < thinking_pos,
        "thinking indicator should be below the prompt: prompt={prompt_pos}, thinking={thinking_pos}"
    );
}

#[test]
fn live_thinking_indicator_shows_thought_preview_below_prompt() {
    let mut lines: Vec<String> = (0..50).map(|i| format!("history line {i}")).collect();
    lines.push("❯ another user question".into());
    lines.push("⋯ searching the code for details".into());
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut app = sample_app(&refs);
    let text = render(&mut app);
    let sq = squash(&text);
    let prompt_pos = sq
        .find("anotheruserquestion")
        .expect("user prompt in squashed text");
    let thinking_pos = sq
        .find("思考中")
        .expect("thinking indicator in squashed text");
    assert!(
        prompt_pos < thinking_pos,
        "thinking indicator should be below the prompt: prompt={prompt_pos}, thinking={thinking_pos}"
    );
    assert!(
        sq.contains("searchingthecode"),
        "thought preview should appear in live indicator: {sq}"
    );
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
    let (_, three_bus) = render_size(&mut app, 120, 40);
    assert!(three_bus.contains("Sessions") && three_bus.contains("worker"));
    assert!(!three_bus.contains("Project ·"));

    app.right_bus = false;
    let (_, three) = render_size(&mut app, 120, 40);
    assert!(three.contains("Sessions") && three.contains("Project ·"));

    app.layout = LayoutMode::Two;
    let (_, two) = render_size(&mut app, 120, 40);
    assert!(two.contains("Sessions") && !two.contains("Project ·"));

    app.layout = LayoutMode::Tabs;
    let (term, tabs) = render_size(&mut app, 120, 40);
    assert!(!tabs.contains("Sessions") && !tabs.contains("Project ·"));
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
    for (sel, expected_label) in [(0, "worker"), (5, "label-04"), (12, "label-11")] {
        app.selected = sel;
        let (term, _) = render_size(&mut app, 50, 20);
        assert!(row_text(&term, 2).contains(expected_label));
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
    assert!(!text.contains("Flow"));
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
    assert!(text.contains("Flow") && !squash(&text).contains("未讀"));
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
        filter_cursor: 2,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    };
    let vis = p.visible();
    assert!(vis
        .iter()
        .all(|(l, v)| l.contains("op") || v.contains("op")));
    assert!(!vis.is_empty());
}

#[test]
fn antigravity_models_are_offered_in_choices_and_command_candidates() {
    let choices = crate::menu::model_choices(&AgentKind::Antigravity);
    assert_eq!(choices[0].1, "");
    assert!(choices.iter().any(|(_, v)| v == "gemini-4-neon"));
    assert!(choices.iter().any(|(_, v)| v == "gemini-3.8-flash"));

    let mut app = test_app();
    let id = SessionId::from("ses_agy_test");
    let mut s = sample_app(&[]).state.sessions.remove("ses_test").unwrap();
    s.id = id.clone();
    s.kind = AgentKind::Antigravity;
    s.status = SessionStatus::Idle;
    app.state.sessions.insert(id.clone(), s);
    app.selected = 0;

    let (_, cands) = crate::commands::arg_candidates(&app, ":model ").unwrap();
    assert!(
        cands.iter().any(|(v, _)| v == "gemini-4-neon"),
        "candidates should offer gemini-4-neon, got {cands:?}"
    );
    assert!(
        cands.iter().any(|(v, _)| v == "gemini-3.8-flash"),
        "candidates should offer gemini-3.8-flash, got {cands:?}"
    );
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
    tx.send(ertangle_adapter::models::ModelQuery {
        models: vec![
            (
                "gemini-4-argon-medium  (Gemini 4 Argon (Medium))".to_string(),
                "gemini-4-argon-medium".to_string(),
            ),
            (
                "gemini-4-argon-high  (Gemini 4 Argon (High))".to_string(),
                "gemini-4-argon-high".to_string(),
            ),
            ("plain".to_string(), "plain".to_string()),
        ],
        problem: None,
    })
    .unwrap();
    let mut p = crate::menu::Picker {
        stage: crate::menu::PickStage::Agent,
        resume_scope: Default::default(),
        title: "t".into(),
        items: vec![],
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: Some(rx),
        loading: true,
        group_effort: true,
        variants: Default::default(),
        load_error: None,
    };
    p.poll_pending();
    let values: Vec<_> = p.items.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(values, vec!["gemini-4-argon", "plain"]);
    assert_eq!(p.variants["gemini-4-argon"], vec!["medium", "high"]);
    assert_eq!(p.items[0].0, "gemini-4-argon  (Gemini 4 Argon)");
    assert_eq!(p.load_error, None);
}

#[test]
fn model_query_problem_is_shown_in_picker() {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(ertangle_adapter::models::ModelQuery {
        models: vec![],
        problem: Some("`agy models` failed".into()),
    })
    .unwrap();
    let mut app = App::new();
    app.picker = Some(crate::menu::Picker {
        stage: crate::menu::PickStage::Model(AgentKind::Antigravity),
        resume_scope: Default::default(),
        title: "t".into(),
        items: vec![("default".into(), String::new())],
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: Some(rx),
        loading: true,
        group_effort: true,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
    if let Some(p) = app.picker.as_mut() {
        p.poll_pending();
        assert!(!p.loading);
        assert!(p.load_error.is_some());
    }
    let screen = render(&mut app);
    assert!(screen.contains("agy models"), "畫面沒有顯示查詢問題");
}

// ---- 附件 (@檔案／圖片)----

fn temp_dir_with(files: &[(&str, &[u8])]) -> std::path::PathBuf {
    let d = ertangle_test_support::temp_dir("ertangle-att");
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
    let dir = ertangle_test_support::temp_dir("ertangle-hist");
    let path = dir.join("h.jsonl");
    let mut h = crate::history::History::load(Some(path.clone()));
    h.push("第一則 \"quoted\"");
    h.push("second");
    let mut h2 = crate::history::History::load(Some(path));
    assert_eq!(h2.prev("").as_deref(), Some("second"));
    assert_eq!(h2.prev("").as_deref(), Some("第一則 \"quoted\""));
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
        ended_ms: None,
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
    assert_eq!(ids.len(), 2);
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
    let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
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
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.input, "y");
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
    // "needle" only exists in an earlier thought line that was folded away; the fold
    // preview contains only the last thought, so it must not be searchable.
    let mut app = sample_app(&[
        "💭 hidden needle",
        "💭 secret thought",
        "visible secret",
        "plain",
    ]);
    let (mut w, _peer) = pair_writer();
    render(&mut app);
    execute_command(&mut app, &mut w, "find needle").unwrap();
    assert_eq!(
        app.search.as_ref().unwrap().matches.len(),
        0,
        "folded-away thought lines are not searchable"
    );
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();
    execute_command(&mut app, &mut w, "find visible").unwrap();
    assert_eq!(app.search.as_ref().unwrap().matches.len(), 1);
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
        ended_ms: None,
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
        ended_ms: None,
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
        ended_ms: None,
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
    app.show_ended = true;
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
    let dir = ertangle_test_support::temp_dir("ertangle-settings");
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);

    // 不要用 sample_app：它選取 opencode，會觸發背景 CLI 查詢並在外面的設定目錄寫入。
    let mut app = test_app();
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

    // 自動 compact：開關預設 on → off；門檻預設 80 → 右鍵切到 90
    let cfg_path = dir.join("ertangle").join("config.toml");
    let idx = |item| {
        crate::settings::ITEMS
            .iter()
            .position(|i| *i == item)
            .unwrap()
    };
    app.settings.as_mut().unwrap().cursor = idx(crate::settings::Item::AutoCompact);
    crate::settings::handle_key(&mut app, None, key(KeyCode::Char(' ')));
    let cfg = ertangle_daemon::config::Config::load_file(&cfg_path);
    assert!(!cfg.auto_compact.enabled());
    app.settings.as_mut().unwrap().cursor = idx(crate::settings::Item::AutoCompactThreshold);
    crate::settings::handle_key(&mut app, None, key(KeyCode::Right));
    let cfg = ertangle_daemon::config::Config::load_file(&cfg_path);
    assert_eq!(cfg.auto_compact.threshold_percent(), 90);

    crate::settings::handle_key(&mut app, None, key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
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
    app.home_project = "/work/app".into();
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
    let t = ertangle_test_support::temp_dir("tuiskill");
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

#[test]
fn quota_ui_clears_exhausted_after_window_resets() {
    use ertangle_core::{Quota, QuotaWindow};
    let now = 200;
    let mut q = Quota {
        agent: "claude".into(),
        windows: vec![
            QuotaWindow {
                name: "5h".into(),
                used_percent: 100.0,
                resets_at_ms: Some(100), // reset in the past
            },
            QuotaWindow {
                name: "7d".into(),
                used_percent: 51.0,
                resets_at_ms: Some(500), // still active
            },
        ],
        reached: true,
        warning: false,
        updated_ms: 50,
    };

    let text = |lines: Vec<ratatui::text::Line>| {
        lines
            .into_iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };

    // When 5h reset and 7d at 51%, agent_lines does not show "✗ 額度已滿"
    let l = text(crate::quota::agent_lines("claude", Some(&q), now));
    assert!(!l[0].contains("✗ 額度已滿"), "{:?}", l[0]);

    // When 7d is 100%, agent_lines shows "✗ 額度已滿"
    q.windows[1].used_percent = 100.0;
    let l_full = text(crate::quota::agent_lines("claude", Some(&q), now));
    assert!(l_full[0].contains("✗ 額度已滿"), "{:?}", l_full[0]);
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
    let dir_flag = ertangle_test_support::temp_dir("flag");
    let dir_cwd = ertangle_test_support::temp_dir("cwd");
    let canon_flag = dir_flag.canonicalize().unwrap();
    let canon_cwd = dir_cwd.canonicalize().unwrap();

    // 1. 旗標優先於啟動目錄
    assert_eq!(
        crate::app::resolve_active_project(Some(&dir_flag), &dir_cwd),
        canon_flag
    );
    // 2. 旗標無效就用啟動目錄
    let bad_flag = tmp.join("nonexistent_flag_dir");
    assert_eq!(
        crate::app::resolve_active_project(Some(&bad_flag), &dir_cwd),
        canon_cwd
    );
    // 3. 沒有旗標：用啟動目錄 (不會被先前切換過的專案蓋掉)
    assert_eq!(
        crate::app::resolve_active_project(None, &dir_cwd),
        canon_cwd
    );
}

#[test]
fn test_app_set_active_project_state_transition() {
    let mut app = test_app();
    let tmp = std::env::temp_dir();
    let target = ertangle_test_support::temp_dir("proj_target");
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
        ended_ms: None,
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
        ended_ms: None,
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
    let p_dir = ertangle_test_support::temp_dir("open_proj");
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
        ended_ms: None,
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
        group: None,
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

#[test]
fn test_q_key_quits_even_when_question_is_pending() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    app.state
        .sessions
        .insert("ses_q".into(), make_test_session("ses_q"));
    app.selected = 0;

    let q = ertangle_core::UserQuestion {
        id: "qst_pending".into(),
        session: "ses_q".into(),
        question: "Pending question".into(),
        header: None,
        options: Vec::new(),
        multi_select: false,
        custom: true,
        group: None,
    };
    app.state.pending_questions.insert("qst_pending".into(), q);

    assert!(app.selected_question().is_some());

    // Press 'q' in normal mode: must quit, not open picker!
    let q_key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    crate::input::handle_key(&mut app, &mut w, q_key).unwrap();
    assert!(
        app.should_quit,
        "q key must set should_quit even when a question is pending"
    );
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn test_ctrl_c_quits_in_question_picker() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let q = ertangle_core::UserQuestion {
        id: "qst_pick_quit".into(),
        session: "ses_q".into(),
        question: "Question".into(),
        header: None,
        options: vec![ertangle_core::QuestionOption::plain("Opt 1")],
        multi_select: false,
        custom: true,
        group: None,
    };
    crate::menu::start_question_picker(&mut app, &q);
    assert_eq!(app.mode, Mode::Pick);

    // Press Ctrl+C in picker
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    crate::menu::handle_pick_key(&mut app, &mut w, ctrl_c).unwrap();
    assert!(app.should_quit, "Ctrl+C in picker must set should_quit");
}

#[test]
fn test_multiple_questions_per_session_answered_sequentially() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    app.state
        .sessions
        .insert("ses_multi_q".into(), make_test_session("ses_multi_q"));
    app.selected = 0;

    let q1 = ertangle_core::UserQuestion {
        id: "qst_seq_1".into(),
        session: "ses_multi_q".into(),
        question: "Question 1".into(),
        header: Some("First".into()),
        options: vec![ertangle_core::QuestionOption::plain("Choice A")],
        multi_select: false,
        custom: true,
        group: None,
    };
    let q2 = ertangle_core::UserQuestion {
        id: "qst_seq_2".into(),
        session: "ses_multi_q".into(),
        question: "Question 2".into(),
        header: Some("Second".into()),
        options: vec![ertangle_core::QuestionOption::plain("Choice B")],
        multi_select: false,
        custom: true,
        group: None,
    };

    app.state
        .pending_questions
        .insert("qst_seq_1".into(), q1.clone());
    app.state
        .pending_questions
        .insert("qst_seq_2".into(), q2.clone());

    // First question is available
    let active_q1 = app.selected_question().unwrap();
    assert_eq!(active_q1.id, "qst_seq_1");

    // Open picker for q1 and submit
    crate::menu::start_question_picker(&mut app, &active_q1);
    let enter_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    crate::menu::handle_pick_key(&mut app, &mut w, enter_key).unwrap();
    assert_eq!(app.mode, Mode::Normal);

    // Simulate daemon resolving q1 and removing it from state
    app.state.pending_questions.remove("qst_seq_1");

    // Next question is now available on the same session
    let active_q2 = app.selected_question().unwrap();
    assert_eq!(active_q2.id, "qst_seq_2");

    // Open picker for q2 and submit
    crate::menu::start_question_picker(&mut app, &active_q2);
    crate::menu::handle_pick_key(&mut app, &mut w, enter_key).unwrap();
    app.state.pending_questions.remove("qst_seq_2");

    assert!(app.selected_question().is_none());
}

fn make_test_session(id: &str) -> Session {
    Session {
        id: id.into(),
        kind: AgentKind::OpenCode,
        label: format!("worker/{id}"),
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
        ended_ms: None,
    }
}

// ---- question picker usability ----

fn qpick_q(
    id: &str,
    multi: bool,
    custom: bool,
    group: Option<&str>,
) -> ertangle_core::UserQuestion {
    ertangle_core::UserQuestion {
        id: id.into(),
        session: "ses_q".into(),
        question: format!("question {id}"),
        header: None,
        options: vec![
            ertangle_core::QuestionOption::plain("Vanilla"),
            ertangle_core::QuestionOption::plain("Chocolate"),
            ertangle_core::QuestionOption::plain("Mango"),
        ],
        multi_select: multi,
        custom,
        group: group.map(str::to_string),
    }
}

fn qpick_key(app: &mut App, w: &mut HostRouter, code: KeyCode) {
    crate::menu::handle_pick_key(app, w, KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
}

fn qpick_type(app: &mut App, w: &mut HostRouter, text: &str) {
    for c in text.chars() {
        qpick_key(app, w, KeyCode::Char(c));
    }
}

fn qpick_sent(peer: &mut std::os::unix::net::UnixStream) -> String {
    use std::io::Read;
    let mut buf = String::new();
    let _ = peer.read_to_string(&mut buf);
    buf
}

#[test]
fn question_picker_custom_answer_keeps_spaces_and_is_not_filtered() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    let q = qpick_q("qst_c", false, true, None);
    crate::menu::start_question_picker(&mut app, &q);
    // Jump to the custom row (4th) and type text that partially matches "Vanilla".
    qpick_key(&mut app, &mut w, KeyCode::Char('4'));
    qpick_type(&mut app, &mut w, "van with spaces");
    // Typing never filters the option list.
    assert_eq!(app.picker.as_ref().unwrap().visible().len(), 3);
    qpick_key(&mut app, &mut w, KeyCode::Enter);
    assert!(app.picker.is_none());
    let sent = qpick_sent(&mut peer);
    assert!(sent.contains("van with spaces"), "{sent}");
    assert!(!sent.contains("\"Vanilla\""), "{sent}");
}

#[test]
fn question_picker_no_custom_row_when_custom_disabled() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    crate::menu::start_question_picker(&mut app, &qpick_q("qst_n", false, false, None));
    // '4' is out of range; typed text is ignored; Enter sends the highlighted option.
    qpick_type(&mut app, &mut w, "4xyz");
    assert!(app.picker.is_some());
    qpick_key(&mut app, &mut w, KeyCode::Down);
    qpick_key(&mut app, &mut w, KeyCode::Enter);
    assert!(qpick_sent(&mut peer).contains("Chocolate"));
}

#[test]
fn question_picker_digit_selects_single_option() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    crate::menu::start_question_picker(&mut app, &qpick_q("qst_d", false, true, None));
    qpick_key(&mut app, &mut w, KeyCode::Char('3'));
    assert!(app.picker.is_none());
    assert!(qpick_sent(&mut peer).contains("Mango"));
}

#[test]
fn question_picker_multi_select_with_custom_and_fallback() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    crate::menu::start_question_picker(&mut app, &qpick_q("qst_m", true, true, None));
    qpick_key(&mut app, &mut w, KeyCode::Char('3')); // toggles Mango
    qpick_key(&mut app, &mut w, KeyCode::Char('1')); // toggles Vanilla
    qpick_key(&mut app, &mut w, KeyCode::Char('4')); // custom mode
    qpick_type(&mut app, &mut w, "plus me");
    qpick_key(&mut app, &mut w, KeyCode::Enter);
    let sent = qpick_sent(&mut peer);
    let (v, m, c) = (
        sent.find("Vanilla").unwrap(),
        sent.find("Mango").unwrap(),
        sent.find("plus me").unwrap(),
    );
    assert!(v < m && m < c, "options in order, custom last: {sent}");

    // Nothing checked: Enter sends the highlighted option.
    crate::menu::start_question_picker(&mut app, &qpick_q("qst_m2", true, true, None));
    qpick_key(&mut app, &mut w, KeyCode::Down);
    qpick_key(&mut app, &mut w, KeyCode::Enter);
    let sent = qpick_sent(&mut peer);
    assert!(
        sent.contains("Chocolate") && !sent.contains("Vanilla"),
        "{sent}"
    );
}

#[test]
fn question_picker_esc_keeps_question_pending() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    let q = qpick_q("qst_e", false, true, None);
    app.state.pending_questions.insert(q.id.clone(), q.clone());
    crate::menu::start_question_picker(&mut app, &q);
    qpick_key(&mut app, &mut w, KeyCode::Esc);
    assert!(app.picker.is_none());
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.state.pending_questions.contains_key("qst_e"));
    assert!(qpick_sent(&mut peer).is_empty());
}

#[test]
fn question_picker_custom_mode_esc_returns_to_options() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    crate::menu::start_question_picker(&mut app, &qpick_q("qst_x", false, true, None));
    qpick_key(&mut app, &mut w, KeyCode::Char('4'));
    qpick_type(&mut app, &mut w, "abc");
    qpick_key(&mut app, &mut w, KeyCode::Esc);
    assert!(app.picker.is_some());
    qpick_key(&mut app, &mut w, KeyCode::Char('2'));
    assert!(qpick_sent(&mut peer).contains("Chocolate"));
}

#[test]
fn question_picker_group_walks_sequentially_and_sends_together() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    app.state
        .sessions
        .insert("ses_q".into(), make_test_session("ses_q"));
    app.selected = 0;
    let q2 = qpick_q("qst_g2", false, true, Some("g"));
    let q1 = qpick_q("qst_g1", false, true, Some("g"));
    app.state.pending_questions.insert(q2.id.clone(), q2);
    app.state.pending_questions.insert(q1.id.clone(), q1);
    // Deterministic order: lowest id first.
    let first = app.selected_question().unwrap();
    assert_eq!(first.id, "qst_g1");
    crate::menu::start_question_picker(&mut app, &first);
    assert!(app.picker.as_ref().unwrap().title.contains("1/2"));
    qpick_key(&mut app, &mut w, KeyCode::Char('1'));
    // Not sent yet: still walking through the group.
    assert!(qpick_sent(&mut peer).is_empty());
    assert!(app.picker.as_ref().unwrap().title.contains("2/2"));
    qpick_key(&mut app, &mut w, KeyCode::Char('3'));
    assert!(app.picker.is_none());
    let sent = qpick_sent(&mut peer);
    let (a, b) = (sent.find("qst_g1").unwrap(), sent.find("qst_g2").unwrap());
    assert!(a < b);
    assert!(sent.contains("Vanilla") && sent.contains("Mango"), "{sent}");
}

#[test]
fn question_picker_group_skip_sends_empty_answer_but_not_all_skipped() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    let q1 = qpick_q("qst_s1", false, true, Some("g"));
    let q2 = qpick_q("qst_s2", false, true, Some("g"));
    app.state
        .pending_questions
        .insert(q1.id.clone(), q1.clone());
    app.state.pending_questions.insert(q2.id.clone(), q2);
    // Skipping every question is refused on the last one.
    crate::menu::start_question_picker(&mut app, &q1);
    qpick_key(&mut app, &mut w, KeyCode::Char('s'));
    qpick_key(&mut app, &mut w, KeyCode::Char('s'));
    assert!(app.picker.is_some());
    assert!(qpick_sent(&mut peer).is_empty());
    qpick_key(&mut app, &mut w, KeyCode::Char('2'));
    assert!(app.picker.is_none());
    let sent = qpick_sent(&mut peer);
    assert_eq!(sent.matches("answer_question").count(), 2, "{sent}");
    assert!(sent.contains("Chocolate"), "{sent}");
    assert!(sent.contains("\"answers\":[]"), "{sent}");
}

#[test]
fn question_picker_group_free_form_only_empty_enter_skips() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    let q1 = qpick_q("qst_f1", false, false, Some("g"));
    let mut q2 = qpick_q("qst_f2", false, true, Some("g"));
    q2.options.clear();
    app.state
        .pending_questions
        .insert(q1.id.clone(), q1.clone());
    app.state.pending_questions.insert(q2.id.clone(), q2);
    crate::menu::start_question_picker(&mut app, &q1);
    qpick_key(&mut app, &mut w, KeyCode::Char('1'));
    // Free-form-only optional field: empty Enter skips and finishes the group.
    qpick_key(&mut app, &mut w, KeyCode::Enter);
    assert!(app.picker.is_none());
    let sent = qpick_sent(&mut peer);
    assert_eq!(sent.matches("answer_question").count(), 2, "{sent}");
    assert!(sent.contains("\"answers\":[]"), "{sent}");
}

#[test]
fn question_picker_group_all_empty_is_not_sent() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    let mut q1 = qpick_q("qst_z1", false, true, Some("g"));
    q1.options.clear();
    let mut q2 = q1.clone();
    q2.id = "qst_z2".into();
    app.state
        .pending_questions
        .insert(q1.id.clone(), q1.clone());
    app.state.pending_questions.insert(q2.id.clone(), q2);
    crate::menu::start_question_picker(&mut app, &q1);
    qpick_key(&mut app, &mut w, KeyCode::Enter); // skip first
    qpick_key(&mut app, &mut w, KeyCode::Enter); // last: refused
    assert!(app.picker.is_some());
    assert!(qpick_sent(&mut peer).is_empty());
    qpick_type(&mut app, &mut w, "hello world");
    qpick_key(&mut app, &mut w, KeyCode::Enter);
    assert!(app.picker.is_none());
    let sent = qpick_sent(&mut peer);
    assert!(
        sent.contains("hello world") && sent.contains("\"answers\":[]"),
        "{sent}"
    );
}

fn auto_open_app() -> (App, ertangle_core::UserQuestion) {
    let mut app = test_app();
    app.state
        .sessions
        .insert("ses_q".into(), make_test_session("ses_q"));
    app.selected = 0;
    let q = qpick_q("qst_auto", false, true, None);
    app.state.pending_questions.insert(q.id.clone(), q.clone());
    (app, q)
}

#[test]
fn question_auto_opens_in_normal_mode() {
    let (mut app, _q) = auto_open_app();
    app.auto_open_question();
    assert_eq!(app.mode, Mode::Pick);
    assert!(matches!(
        app.picker.as_ref().map(|p| &p.stage),
        Some(crate::menu::PickStage::Question { .. })
    ));
}

#[test]
fn question_does_not_auto_open_while_typing() {
    let (mut app, _q) = auto_open_app();
    app.mode = Mode::Prompt;
    app.auto_open_question();
    assert_eq!(app.mode, Mode::Prompt);
    assert!(app.picker.is_none());
}

#[test]
fn question_esc_dismisses_and_enter_reopens() {
    let (mut app, _q) = auto_open_app();
    let (mut w, _peer) = pair_writer();
    app.auto_open_question();
    qpick_key(&mut app, &mut w, KeyCode::Esc);
    assert_eq!(app.mode, Mode::Normal);
    app.auto_open_question();
    assert!(app.picker.is_none(), "dismissed question must not re-open");
    // Manual Enter in normal mode re-opens it.
    crate::input::handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Pick);
    // Once the daemon resolves it, the dismissed id is forgotten.
    app.picker = None;
    app.mode = Mode::Normal;
    app.state.pending_questions.clear();
    app.auto_open_question();
    assert!(app.dismissed_questions.is_empty());
}

#[test]
fn question_answered_does_not_reopen_before_daemon_clears_it() {
    let (mut app, _q) = auto_open_app();
    let (mut w, _peer) = pair_writer();
    app.auto_open_question();
    qpick_key(&mut app, &mut w, KeyCode::Char('1'));
    app.auto_open_question();
    assert!(app.picker.is_none());
}

#[test]
fn question_docked_above_input_hides_selected_banner() {
    use ratatui::{backend::TestBackend, Terminal};
    let (mut app, _q) = auto_open_app();
    app.auto_open_question();
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::draw::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    let row = |y: u16| -> String { (0..100).map(|x| buf[(x, y)].symbol()).collect() };
    let all: Vec<String> = (0..30).map(row).collect();
    let top = all
        .iter()
        .position(|r| r.contains("question qst_auto"))
        .unwrap();
    assert!(top > 15, "picker should sit near the bottom, got row {top}");
    assert!(
        !all.iter().any(|r| r.contains("⏸")),
        "banner for the open question must be hidden"
    );
}

#[test]
fn restart_needs_confirmation_while_sessions_are_live() {
    let mut app = sample_app(&["hi"]);
    let (mut w, mut peer) = pair_writer();
    peer.set_nonblocking(true).unwrap();
    crate::commands::execute_command(&mut app, &mut w, "restart").unwrap();
    assert!(app.status.contains(":restart!"), "{}", app.status);
    let mut buf = [0u8; 16];
    assert!(
        std::io::Read::read(&mut peer, &mut buf).is_err(),
        "nothing sent yet"
    );
    crate::commands::execute_command(&mut app, &mut w, "restart!").unwrap();
    assert!(!app.should_quit, "restart keeps the TUI alive to reconnect");
    peer.set_nonblocking(false).unwrap();
    let n = std::io::Read::read(&mut peer, &mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).contains("restart"));
}

fn custom_state(app: &App) -> (String, usize) {
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Question {
            custom_text,
            custom_cursor,
            ..
        } => (custom_text.clone(), *custom_cursor),
        _ => panic!("not a question picker"),
    }
}

fn open_custom(app: &mut App, w: &mut HostRouter) {
    crate::menu::start_question_picker(app, &qpick_q("qst_ed", false, true, None));
    qpick_key(app, w, KeyCode::Char('4'));
}

#[test]
fn question_custom_edits_in_middle_with_cjk() {
    let mut app = test_app();
    let (mut w, _peer) = pair_writer();
    open_custom(&mut app, &mut w);
    qpick_type(&mut app, &mut w, "你好世界");
    qpick_key(&mut app, &mut w, KeyCode::Left);
    qpick_key(&mut app, &mut w, KeyCode::Left);
    qpick_type(&mut app, &mut w, "X");
    assert_eq!(custom_state(&app), ("你好X世界".to_string(), 3));
    qpick_key(&mut app, &mut w, KeyCode::Backspace);
    qpick_key(&mut app, &mut w, KeyCode::Delete);
    assert_eq!(custom_state(&app), ("你好界".to_string(), 2));
}

#[test]
fn question_custom_home_end_and_ctrl_keys() {
    let mut app = test_app();
    let (mut w, _peer) = pair_writer();
    open_custom(&mut app, &mut w);
    qpick_type(&mut app, &mut w, "foo bar baz");
    qpick_key(&mut app, &mut w, KeyCode::Home);
    assert_eq!(custom_state(&app).1, 0);
    qpick_key(&mut app, &mut w, KeyCode::End);
    assert_eq!(custom_state(&app).1, 11);
    // Ctrl+W deletes the previous word.
    crate::menu::handle_pick_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(custom_state(&app), ("foo bar ".to_string(), 8));
    // Ctrl+A then Ctrl+U is a no-op at start; Ctrl+E + Ctrl+U clears.
    crate::menu::handle_pick_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(custom_state(&app), (String::new(), 0));
}

#[test]
fn question_custom_paste_inserts_at_cursor() {
    let mut app = test_app();
    let (mut w, _peer) = pair_writer();
    open_custom(&mut app, &mut w);
    qpick_type(&mut app, &mut w, "ad");
    qpick_key(&mut app, &mut w, KeyCode::Left);
    crate::input::handle_paste(&mut app, "b\nc");
    assert_eq!(custom_state(&app), ("ab cd".to_string(), 4));
}

#[test]
fn question_resolved_remotely_closes_picker() {
    let (mut app, q) = auto_open_app();
    app.auto_open_question();
    assert!(app.picker.is_some());
    app.state.pending_questions.remove(&q.id);
    assert!(crate::menu::reconcile_question_picker(&mut app));
    assert!(app.picker.is_none());
    assert_eq!(app.mode, Mode::Normal);
    // Resolved question must not auto-reopen.
    app.auto_open_question();
    assert!(app.picker.is_none());
}

#[test]
fn question_group_remote_resolution_advances_without_sending() {
    let mut app = test_app();
    let (mut w, mut peer) = pair_writer();
    app.state
        .sessions
        .insert("ses_q".into(), make_test_session("ses_q"));
    app.selected = 0;
    for id in ["qst_g1", "qst_g2", "qst_g3"] {
        let q = qpick_q(id, false, true, Some("g"));
        app.state.pending_questions.insert(q.id.clone(), q);
    }
    let first = app.selected_question().unwrap();
    crate::menu::start_question_picker(&mut app, &first);
    qpick_key(&mut app, &mut w, KeyCode::Char('1'));
    // Now on g2; web resolves g2 and g1.
    app.state.pending_questions.remove("qst_g2");
    app.state.pending_questions.remove("qst_g1");
    assert!(crate::menu::reconcile_question_picker(&mut app));
    let p = app.picker.as_ref().unwrap();
    assert!(
        matches!(&p.stage, crate::menu::PickStage::Question { id, questions, collected, .. }
        if id == "qst_g3" && questions.len() == 1 && collected.is_empty())
    );
    qpick_key(&mut app, &mut w, KeyCode::Char('2'));
    let sent = qpick_sent(&mut peer);
    assert!(sent.contains("qst_g3") && !sent.contains("qst_g1") && !sent.contains("qst_g2"));
}

fn app_with_ended() -> App {
    let mut app = sample_app(&[]);
    app.hosts.clear();
    add_session(&mut app, "ses_done1", "done1", 10);
    add_session(&mut app, "ses_done2", "done2", 11);
    for id in ["ses_done1", "ses_done2"] {
        app.state.sessions.get_mut(id).unwrap().status = SessionStatus::Exited;
    }
    app
}

#[test]
fn ended_sessions_are_folded_by_default_and_counted() {
    let app = app_with_ended();
    assert_eq!(app.ended_count(), 2);
    let ids = app.session_ids();
    assert!(ids.contains(&"ses_test".to_string()));
    assert!(!ids.iter().any(|id| id.starts_with("ses_done")));
}

#[test]
fn ended_group_expands_at_the_bottom_and_toggle_keeps_selection() {
    let mut app = app_with_ended();
    toggle_ended(&mut app, None);
    let ids = app.session_ids();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids[0], "ses_test", "live sessions stay first");
    assert!(ids[1..].iter().all(|id| id.starts_with("ses_done")));

    app.selected = 2;
    let kept = app.selected_id();
    toggle_ended(&mut app, Some(false));
    assert!(!app.show_ended);
    // The selected ended session disappeared: selection falls back to the first row.
    assert_eq!(app.selected, 0);
    assert_ne!(app.selected_id(), kept);
}

#[test]
fn ended_group_header_is_rendered_with_count() {
    let mut app = app_with_ended();
    let (_, collapsed) = render_size(&mut app, 120, 30);
    assert!(
        collapsed.contains("▸ ") && collapsed.contains("(2)"),
        "{collapsed}"
    );
    app.show_ended = true;
    let (_, expanded) = render_size(&mut app, 120, 30);
    assert!(expanded.contains('▾') && expanded.contains("done1"));
}

#[test]
fn prune_command_sends_scoped_request_and_ended_command_toggles() {
    let mut app = app_with_ended();
    let (mut w, peer) = pair_writer();
    app.scope_here = false;
    crate::commands::execute_command(&mut app, &mut w, "prune").unwrap();
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut &peer, &mut buf).ok();
    assert!(
        buf.contains("\"op\":\"prune\"") && buf.contains("null"),
        "{buf}"
    );

    app.scope_here = true;
    crate::commands::execute_command(&mut app, &mut w, "prune").unwrap();
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut &peer, &mut buf).ok();
    assert!(
        buf.contains(&app.home_project),
        "scope_here limits to project: {buf}"
    );

    crate::commands::execute_command(&mut app, &mut w, "ended show").unwrap();
    assert!(app.show_ended);
    crate::commands::execute_command(&mut app, &mut w, "ended hide").unwrap();
    assert!(!app.show_ended);
    assert!(crate::commands::command_hints("prune")
        .iter()
        .any(|(n, _)| *n == "prune"));
}

// ---- T74a: shell command execution (!cmd / :exec cmd / ! shortcut) ----

#[test]
fn tui_bang_prompt_sends_exec_request() {
    use std::io::{BufRead, BufReader};
    let mut app = sample_app(&[]);
    let (mut w, peer) = pair_writer();
    crate::commands::submit_prompt_or_route(&mut app, &mut w, "!  echo hello ").unwrap();
    let mut line = String::new();
    BufReader::new(peer).read_line(&mut line).unwrap();
    match ertangle_protocol::decode_line(&line).unwrap() {
        Request::Exec { session, command } => {
            assert_eq!(session, "ses_test");
            assert_eq!(command, "echo hello");
        }
        other => panic!("expected Exec request, got {other:?}"),
    }
    assert!(app.status.contains("echo hello"), "{}", app.status);
}

#[test]
fn tui_exec_and_bang_command_aliases_send_exec_request() {
    use std::io::{BufRead, BufReader};
    for input in ["exec echo hi", "sh echo hi", "!echo hi", "! echo hi"] {
        let mut app = sample_app(&[]);
        let (mut w, peer) = pair_writer();
        crate::commands::execute_command(&mut app, &mut w, input).unwrap();
        let mut line = String::new();
        BufReader::new(peer).read_line(&mut line).unwrap();
        match ertangle_protocol::decode_line(&line).unwrap() {
            Request::Exec { session, command } => {
                assert_eq!(session, "ses_test", "input: {input}");
                assert_eq!(command, "echo hi", "input: {input}");
            }
            other => panic!("expected Exec request for {input}, got {other:?}"),
        }
    }
}

#[test]
fn tui_empty_shell_command_is_rejected_without_request() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    let (mut w, mut peer) = pair_writer();
    crate::commands::exec_shell_command(&mut app, &mut w, "   ").unwrap();
    let mut byte = [0u8; 1];
    assert!(matches!(
        peer.read(&mut byte),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    ));
    assert!(app.status.contains("命令不可為空"), "{}", app.status);
}

#[test]
fn tui_bang_key_prefills_prompt_shell_mode() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    key(&mut app, &mut w, KeyCode::Char('!'));
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "!");
    assert_eq!(app.input_cursor, 1);
    assert!(app.status.contains("shell 命令"), "{}", app.status);
}

// ---- T76b: Flow view ----

fn flow_node(id: &str, label: &str, depth: usize, status: SessionStatus) -> crate::flow::FlowNode {
    crate::flow::FlowNode {
        id: id.into(),
        label: label.into(),
        status,
        depth,
        unread: 0,
        queued: 0,
        questions: 0,
        permissions: 0,
        tasks: Vec::new(),
    }
}

fn flow_msg_edge(from: &str, to: crate::flow::FlowEndpoint, ts: u64) -> crate::flow::FlowEdge {
    crate::flow::FlowEdge {
        from: from.into(),
        to,
        kind: crate::flow::FlowEdgeKind::Message(ertangle_core::MsgKind::Chat),
        count: 3,
        last_ts: ts,
        preview: "hello there".into(),
    }
}

fn flow_model() -> crate::flow::FlowModel {
    let mut root = flow_node("a", "root-agent", 0, SessionStatus::Busy);
    root.unread = 2;
    root.queued = 1;
    let child = flow_node("b", "child-agent", 1, SessionStatus::WaitingApproval);
    crate::flow::FlowModel {
        summary: crate::flow::FlowSummary {
            live: 2,
            busy: 1,
            waiting: 1,
            messages_in_window: 5,
            ..Default::default()
        },
        nodes: vec![root, child],
        ended_folded: 4,
        edges: vec![
            crate::flow::FlowEdge {
                from: "b".into(),
                to: crate::flow::FlowEndpoint::Unassigned,
                kind: crate::flow::FlowEdgeKind::BlockedBy {
                    task: "T9".into(),
                    dep: "T8".into(),
                },
                count: 1,
                last_ts: 0,
                preview: String::new(),
            },
            flow_msg_edge("a", crate::flow::FlowEndpoint::Session("b".into()), 99_000),
            flow_msg_edge(
                "b",
                crate::flow::FlowEndpoint::Channel("ops".into()),
                10_000,
            ),
            flow_msg_edge("a", crate::flow::FlowEndpoint::Broadcast, 10_000),
        ],
        edges_truncated: 2,
        now_ms: 100_000,
    }
}

fn render_flow(model: &crate::flow::FlowModel, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::flow::draw_flow(f, f.area(), model, 1))
        .unwrap();
    buffer_text(&term)
}

#[test]
fn flow_renders_sections() {
    // Wide glyphs leave filler cells in the buffer dump; compare without spaces.
    let text = render_flow(&flow_model(), 100, 24).replace(' ', "");
    for want in [
        "Flow",
        "2存活",
        "root-agent",
        "✉2",
        "⧗1",
        "4個已結束",
        "┄┄blockedby┄┄▶",
        "T9←T8",
        "(未指派)",
        "#ops",
        "*",
        "chat×3",
        "\"hellothere\"",
        "+2筆更多",
    ] {
        assert!(text.contains(want), "missing {want:?} in {text}");
    }
    assert!(text.contains("⏸child-agent"));
}

#[test]
fn flow_pulse_only_on_recent_edge() {
    let text = render_flow(&flow_model(), 100, 24);
    assert_eq!(text.matches('⚡').count(), 1, "{text}");
}

#[test]
fn flow_narrow_hides_badges_and_preview() {
    let text = render_flow(&flow_model(), 50, 14);
    assert!(text.contains("✉2"));
    assert!(!text.contains("⧗1"));
    assert!(!text.contains("hello there"));
}

#[test]
fn flow_tiny_sizes_do_not_panic() {
    let m = flow_model();
    for (w, h) in [
        (80, 24),
        (50, 12),
        (20, 5),
        (1, 1),
        (3, 2),
        (10, 9),
        (0, 0),
        (200, 3),
    ] {
        let _ = render_flow(&m, w, h);
        let _ = render_flow(&crate::flow::FlowModel::default(), w, h);
    }
}

#[test]
fn flow_f_key_and_command_toggle_restore_view() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.view = CenterView::Diff;
    key(&mut app, &mut w, KeyCode::Char('F'));
    assert_eq!(app.view, CenterView::Flow);
    key(&mut app, &mut w, KeyCode::Char('F'));
    assert_eq!(app.view, CenterView::Diff);
    app.view = CenterView::Tasks;
    crate::commands::execute_command(&mut app, &mut w, "flow").unwrap();
    assert_eq!(app.view, CenterView::Flow);
    crate::commands::execute_command(&mut app, &mut w, "flow").unwrap();
    assert_eq!(app.view, CenterView::Tasks);
}

#[test]
fn flow_esc_returns_and_enter_goes_to_transcript() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.view = CenterView::Worktrees;
    key(&mut app, &mut w, KeyCode::Char('F'));
    key(&mut app, &mut w, KeyCode::Char('j'));
    key(&mut app, &mut w, KeyCode::Down);
    key(&mut app, &mut w, KeyCode::Char('k'));
    assert_eq!(app.view, CenterView::Flow);
    key(&mut app, &mut w, KeyCode::Esc);
    assert_eq!(app.view, CenterView::Worktrees);
    key(&mut app, &mut w, KeyCode::Char('F'));
    key(&mut app, &mut w, KeyCode::Enter);
    assert_eq!(app.view, CenterView::Transcript);
}

#[test]
fn flow_tab_goes_to_transcript_and_cycle_is_unchanged() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.view = CenterView::Flow;
    key(&mut app, &mut w, KeyCode::Tab);
    assert_eq!(app.view, CenterView::Transcript);
    key(&mut app, &mut w, KeyCode::Tab);
    assert_eq!(app.view, CenterView::Diff);
    key(&mut app, &mut w, KeyCode::Tab);
    assert_eq!(app.view, CenterView::Worktrees);
    key(&mut app, &mut w, KeyCode::Tab);
    assert_eq!(app.view, CenterView::Tasks);
    key(&mut app, &mut w, KeyCode::Tab);
    assert_eq!(app.view, CenterView::Transcript);
}

#[test]
fn flow_keeps_bus_toggle() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.view = CenterView::Flow;
    let before = (app.bus_overlay, app.right_bus);
    key(&mut app, &mut w, KeyCode::Char('b'));
    assert_ne!((app.bus_overlay, app.right_bus), before);
    assert_eq!(app.view, CenterView::Flow);
}

#[test]
fn flow_view_renders_through_main_draw() {
    let mut app = sample_app(&[]);
    app.view = CenterView::Flow;
    let text = render(&mut app);
    assert!(text.contains("Flow"));
}

#[test]
fn flow_real_model_lists_session_and_enter_selects_it() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    key(&mut app, &mut w, KeyCode::Char('F'));
    let text = render(&mut app);
    assert!(
        text.contains("worker"),
        "node label rendered from real model"
    );
    key(&mut app, &mut w, KeyCode::Enter);
    assert_eq!(app.view, CenterView::Transcript);
    assert_eq!(app.selected_id().as_deref(), Some("ses_test"));
}

#[test]
fn flow_l_toggles_graph_and_list() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    key(&mut app, &mut w, KeyCode::Char('F'));
    let graph = render(&mut app);
    assert!(graph.contains('╔'), "graph mode draws the cursor box");
    key(&mut app, &mut w, KeyCode::Char('l'));
    assert!(app.flow_list);
    let list = render(&mut app);
    assert!(!list.contains('╔'));
    assert!(list.contains("worker"));
    key(&mut app, &mut w, KeyCode::Char('l'));
    assert!(!app.flow_list);
}

fn question_for(session: &str, id: &str) -> ertangle_core::UserQuestion {
    ertangle_core::UserQuestion {
        id: id.into(),
        session: session.into(),
        question: "Allow?".into(),
        header: None,
        options: Vec::new(),
        multi_select: false,
        custom: true,
        group: None,
    }
}

#[test]
fn w_jumps_to_next_pending_session_and_cycles() {
    let mut app = sample_app(&["hi"]);
    add_session(&mut app, "ses_a", "a", 1);
    add_session(&mut app, "ses_b", "b", 2);
    add_session(&mut app, "ses_c", "c", 3);
    let ids = app.session_ids();
    let pos = |id: &str| ids.iter().position(|s| s == id).unwrap();
    app.selected = pos("ses_a");

    assert!(!app.jump_to_pending(), "沒有待處理時不跳轉");
    assert_eq!(app.selected, pos("ses_a"));

    app.state
        .pending_questions
        .insert("q1".into(), question_for("ses_c", "q1"));
    app.state.pending_permissions.insert(
        "r1".into(),
        ertangle_core::PermissionRequest {
            req: "r1".into(),
            session: "ses_b".into(),
            title: "Bash".into(),
            detail: "x".into(),
        },
    );
    app.mode = Mode::Command; // 避免自動開啟選擇器干擾順序判斷
    assert!(app.jump_to_pending());
    let first = app.selected_id().unwrap();
    assert!(app.jump_to_pending());
    let second = app.selected_id().unwrap();
    assert!(app.jump_to_pending());
    let third = app.selected_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(first, third, "只有兩個待處理，第三次要循環回第一個");
    assert!(["ses_b", "ses_c"].contains(&first.as_str()));
    assert!(["ses_b", "ses_c"].contains(&second.as_str()));
}

#[test]
fn jump_to_pending_reopens_dismissed_question_picker() {
    let mut app = sample_app(&["hi"]);
    add_session(&mut app, "ses_q", "q", 5);
    app.state
        .pending_questions
        .insert("q1".into(), question_for("ses_q", "q1"));
    app.dismissed_questions.insert("q1".into());
    app.mode = Mode::Normal;
    assert!(app.jump_to_pending());
    assert!(app.picker.is_some(), "跳過去應直接開啟提問選擇器");
}

#[test]
fn subcommand_candidates_complete_via_tab_and_hints() {
    let mut app = test_app();
    for (input, expected) in [
        (":project op", "open"),
        (":scope a", "all"),
        (":layout t", "tabs"),
        (":layout a", "auto"),
        (":notify o", "on"),
        (":web st", "status"),
        (":ended sh", "show"),
        (":queue c", "clear"),
        (":task-status bl", "blocked"),
        (":merge --dr", "--dry-run"),
    ] {
        let (_, cands) = crate::commands::arg_candidates(&app, input).unwrap();
        assert!(
            cands.iter().any(|(v, _)| v == expected),
            "input {input} should offer {expected}, got {cands:?}"
        );
        app.input = input.to_string();
        app.input_cursor = app.input.chars().count();
        let hints = crate::commands::hints_with_native(&app);
        assert!(
            hints.iter().any(|(v, _)| v == expected),
            "hints for {input} should contain {expected}"
        );
    }

    for (input, expected) in [
        (":project op", ":project open "),
        (":scope a", ":scope all "),
    ] {
        app.input = input.to_string();
        app.input_cursor = app.input.chars().count();
        crate::input::complete_command_for_test(&mut app);
        assert_eq!(app.input, expected);
    }
}

#[test]
fn project_open_path_candidates_complete_directories() {
    let app = test_app();
    let tmp = ertangle_test_support::temp_dir("path_cands");
    let alpha = tmp.join("alpha");
    let beta = tmp.join("beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();

    let prefix = format!(":project open {}/a", tmp.display());
    let (_, cands) = crate::commands::arg_candidates(&app, &prefix).unwrap();
    assert!(
        cands
            .iter()
            .any(|(v, _)| v.as_str() == format!("{}/alpha/", tmp.display())),
        "expected alpha directory candidate, got {cands:?}"
    );
}

#[test]
fn space_p_opens_project_picker() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let root = crate::menu::menu_items(&[]).unwrap().1;
    assert!(
        root.iter().any(|item| item.key == 'P'),
        "SPC P must be listed in the root which-key menu"
    );
    key(&mut app, &mut w, KeyCode::Char(' '));
    assert_eq!(app.mode, Mode::Menu);
    key(&mut app, &mut w, KeyCode::Char('P'));
    assert!(matches!(
        app.picker.as_ref().map(|p| &p.stage),
        Some(crate::menu::PickStage::Project)
    ));
}

#[test]
fn project_folder_browser_is_separate_from_project_list() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let tmp = ertangle_test_support::temp_dir("folder_pick");
    let child = tmp.join("child_dir");
    std::fs::create_dir_all(&child).unwrap();
    app.active_project = tmp.clone();

    // 專案清單不再夾帶「瀏覽資料夾」列。
    crate::commands::execute_command(&mut app, &mut w, "project open").unwrap();
    assert!(matches!(
        app.picker.as_ref().map(|p| &p.stage),
        Some(crate::menu::PickStage::Project)
    ));
    assert!(app
        .picker
        .as_ref()
        .unwrap()
        .items
        .iter()
        .all(|(l, _)| !l.contains("瀏覽資料夾")));
    key(&mut app, &mut w, KeyCode::Esc);

    // Space p 直接開資料夾瀏覽器。
    key(&mut app, &mut w, KeyCode::Char(' '));
    key(&mut app, &mut w, KeyCode::Char('p'));
    assert!(matches!(
        &app.picker.as_ref().unwrap().stage,
        crate::menu::PickStage::Directory { current } if current == &tmp
    ));

    let target = app
        .picker
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|(_, v)| v == "child_dir")
        .unwrap();
    app.picker.as_mut().unwrap().cursor = target;
    key(&mut app, &mut w, KeyCode::Enter);
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &child),
        _other => panic!("expected directory stage"),
    }

    // Backspace 回上層；Esc 直接關閉選擇器。
    key(&mut app, &mut w, KeyCode::Backspace);
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &tmp),
        _other => panic!("expected directory stage"),
    }
    key(&mut app, &mut w, KeyCode::Esc);
    assert!(app.picker.is_none());
    assert_eq!(app.mode, Mode::Normal);

    // `:project browse` 同樣直接開瀏覽器，並可選取目前資料夾。
    crate::commands::execute_command(&mut app, &mut w, "project browse").unwrap();
    let select = app
        .picker
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|(_, v)| v == crate::menu::SELECT_VALUE)
        .unwrap();
    app.picker.as_mut().unwrap().cursor = select;
    key(&mut app, &mut w, KeyCode::Enter);
    assert!(app.picker.is_none());
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.active_project, tmp.canonicalize().unwrap());
}

#[test]
fn directory_items_put_use_current_folder_first() {
    let tmp = ertangle_test_support::temp_dir("dir_items");
    std::fs::create_dir_all(tmp.join("sub")).unwrap();
    let items = crate::menu::directory_items(&tmp);
    assert_eq!(items[0].1, crate::menu::SELECT_VALUE);
    let parent = items
        .iter()
        .position(|(_, v)| v == crate::menu::PARENT_VALUE)
        .unwrap();
    if let Some(home) = items.iter().position(|(_, v)| v == crate::menu::HOME_VALUE) {
        assert_eq!(home, 1, "家目錄緊接在選取列之後");
        assert_eq!(parent, home + 1, "家目錄在上層之上");
    } else {
        assert_eq!(parent, 1);
    }
    assert!(items.iter().any(|(_, v)| v == "sub"));
}

fn open_browser_at(app: &mut App, w: &mut HostRouter, dir: &std::path::Path) {
    app.active_project = dir.to_path_buf();
    crate::commands::execute_command(app, w, "project browse").unwrap();
}

#[test]
fn folder_browser_filters_subfolders_and_backspace_edits_filter() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let tmp = ertangle_test_support::temp_dir("dir_filter");
    for d in ["alpha", "beta", "alphabet"] {
        std::fs::create_dir_all(tmp.join(d)).unwrap();
    }
    open_browser_at(&mut app, &mut w, &tmp);

    for c in "alph".chars() {
        key(&mut app, &mut w, KeyCode::Char(c));
    }
    let visible: Vec<String> = app
        .picker
        .as_ref()
        .unwrap()
        .visible()
        .iter()
        .map(|(_, v)| v.clone())
        .collect();
    assert_eq!(
        visible,
        ["alpha", "alphabet"],
        "只比對資料夾名稱，捷徑列隱藏"
    );

    // Backspace 刪過濾字，不是回上層。
    key(&mut app, &mut w, KeyCode::Backspace);
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &tmp),
        _ => panic!("expected directory stage"),
    }
    assert_eq!(app.picker.as_ref().unwrap().filter, "alp");

    // Esc 先清過濾；過濾已空時 Esc 才關閉。
    key(&mut app, &mut w, KeyCode::Esc);
    assert!(app.picker.as_ref().unwrap().filter.is_empty());
    assert!(
        app.picker.as_ref().unwrap().visible().len() >= 5,
        "捷徑列與子資料夾都回來"
    );
    key(&mut app, &mut w, KeyCode::Esc);
    assert!(app.picker.is_none());

    // Enter 進入過濾後的第一個資料夾。
    open_browser_at(&mut app, &mut w, &tmp);
    for c in "beta".chars() {
        key(&mut app, &mut w, KeyCode::Char(c));
    }
    key(&mut app, &mut w, KeyCode::Enter);
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &tmp.join("beta")),
        _ => panic!("expected directory stage"),
    }
}

#[test]
fn folder_browser_tilde_shortcut_jumps_to_home() {
    let Some(home) = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|h| h.is_dir())
    else {
        return;
    };
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let tmp = ertangle_test_support::temp_dir("dir_home");

    // 捷徑列。
    open_browser_at(&mut app, &mut w, &tmp);
    let row = app
        .picker
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|(_, v)| v == crate::menu::HOME_VALUE)
        .expect("應有家目錄捷徑列");
    app.picker.as_mut().unwrap().cursor = row;
    key(&mut app, &mut w, KeyCode::Enter);
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &home),
        _ => panic!("expected directory stage"),
    }

    // 輸入 ~ 後 Enter。
    open_browser_at(&mut app, &mut w, &tmp);
    key(&mut app, &mut w, KeyCode::Char('~'));
    key(&mut app, &mut w, KeyCode::Enter);
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &home),
        _ => panic!("expected directory stage"),
    }
}

#[test]
fn directory_items_share_a_two_column_gutter() {
    let tmp = ertangle_test_support::temp_dir("dir_align");
    std::fs::create_dir_all(tmp.join("sub")).unwrap();
    for (label, _) in crate::menu::directory_items(&tmp) {
        let mut chars = label.chars();
        chars.next();
        assert_eq!(
            chars.next(),
            Some(' '),
            "第二欄必須是空白，名稱才對齊：{label:?}"
        );
        // 用 ASCII 圖示，避免東亞寬度模糊字元在 CJK 終端機佔兩格而錯位。
        assert!(
            label.chars().next().is_some_and(|c| c.is_ascii()),
            "{label:?}"
        );
    }
}

#[test]
fn picker_filter_is_a_text_input_with_cursor_and_placeholder() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let tmp = ertangle_test_support::temp_dir("pick_input");
    open_browser_at(&mut app, &mut w, &tmp);

    // 空輸入框顯示提示，標題不再夾帶「輸入過濾」。
    let (_, text) = render_size(&mut app, 100, 30);
    assert!(
        squash(&text).contains(&squash("輸入過濾資料夾")),
        "placeholder missing"
    );
    assert!(!app.picker.as_ref().unwrap().title.contains("輸入過濾"));

    for c in "ac".chars() {
        key(&mut app, &mut w, KeyCode::Char(c));
    }
    let (_, text) = render_size(&mut app, 100, 30);
    assert!(
        !squash(&text).contains(&squash("輸入過濾資料夾")),
        "打字後提示要消失"
    );

    // 游標左移後在中間插入。
    key(&mut app, &mut w, KeyCode::Left);
    key(&mut app, &mut w, KeyCode::Char('b'));
    assert_eq!(app.picker.as_ref().unwrap().filter, "abc");
    // Home / Delete 刪第一個字；Backspace 在行首不會誤回上層。
    key(&mut app, &mut w, KeyCode::Home);
    key(&mut app, &mut w, KeyCode::Delete);
    assert_eq!(app.picker.as_ref().unwrap().filter, "bc");
    key(&mut app, &mut w, KeyCode::Backspace);
    assert_eq!(app.picker.as_ref().unwrap().filter, "bc");
    match &app.picker.as_ref().unwrap().stage {
        crate::menu::PickStage::Directory { current } => assert_eq!(current, &tmp),
        _ => panic!("expected directory stage"),
    }
    // Ctrl-U 清到行首；貼上插進輸入框。
    key(&mut app, &mut w, KeyCode::End);
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert!(app.picker.as_ref().unwrap().filter.is_empty());
    handle_paste(&mut app, "x\ny");
    assert_eq!(app.picker.as_ref().unwrap().filter, "x y");
}

#[test]
fn folder_browser_hides_home_row_in_home_and_shows_hint_bottom_right() {
    let mut app = test_app();
    let (mut w, _rx) = pair_writer();
    let tmp = ertangle_test_support::temp_dir("dir_hint");

    // 右下角的 Enter 提示：在邊框最後一列，靠右。
    open_browser_at(&mut app, &mut w, &tmp);
    let (term, text) = render_size(&mut app, 100, 30);
    let hint = squash("Enter 進入／選取 · Esc 取消");
    assert!(squash(&text).contains(&hint));
    let buf = term.backend().buffer();
    let bottom_row = (0..buf.area.height)
        .map(|y| row_text(&term, y))
        .find(|r| squash(r).contains(&hint))
        .unwrap();
    assert!(!bottom_row.trim_end().is_empty());
    let hint_col = bottom_row.find("Enter").unwrap();
    assert!(
        hint_col > bottom_row.len() / 2,
        "提示應在選擇器右半部：{bottom_row:?}"
    );
    assert!(!app.picker.as_ref().unwrap().title.contains("Enter"));
    key(&mut app, &mut w, KeyCode::Esc);

    // 家目錄裡不再列 `~/`。
    let Some(home) = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|h| h.is_dir())
    else {
        return;
    };
    open_browser_at(&mut app, &mut w, &home);
    assert!(app
        .picker
        .as_ref()
        .unwrap()
        .items
        .iter()
        .all(|(_, v)| v != crate::menu::HOME_VALUE));
}

#[test]
fn right_pane_renders_flow_graph_and_scoped_title() {
    let mut app = sample_app(&["echo 1"]);
    app.layout = LayoutMode::Three;
    app.right_bus = true;

    // Default scope: "Flow" title and session label in right pane.
    let (_, text) = render_size(&mut app, 140, 40);
    assert!(text.contains("Flow"), "right pane contains Flow: {text}");
    assert!(
        text.contains("Bus / Tasks"),
        "right pane contains Bus / Tasks: {text}"
    );
    assert!(text.contains("worker"), "session node rendered: {text}");

    // Project scope: title reflects project scope.
    app.home_project = "/work/test".into();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.cwd = "/work/test".into();
    }
    app.scope_here = true;
    let (_, scoped_text) = render_size(&mut app, 140, 40);
    assert!(
        squash(&scoped_text).contains(&squash("Flow · 本專案")),
        "scoped title rendered: {scoped_text}"
    );
    assert!(scoped_text.contains("worker"));

    // List mode toggle: flow_list falls back to node list.
    app.flow_list = true;
    let (_, list_text) = render_size(&mut app, 140, 40);
    assert!(squash(&list_text).contains(&squash("Flow · 本專案")));
    assert!(list_text.contains("worker"));
}

#[test]
fn bus_overlay_and_pending_banner_with_flow_pane() {
    let mut app = sample_app(&["echo 1"]);
    app.layout = LayoutMode::Two;
    app.bus_overlay = true;

    // Bus overlay renders Flow pane.
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("Flow"), "overlay contains Flow: {text}");
    assert!(
        text.contains("Bus / Tasks"),
        "overlay contains Bus / Tasks: {text}"
    );
    assert!(text.contains("worker"), "overlay contains session: {text}");

    // Pending permissions / questions stay visible even when right_bus is on in Three mode.
    app.layout = LayoutMode::Three;
    app.right_bus = true;
    app.state.pending_permissions.insert(
        "p1".into(),
        ertangle_core::PermissionRequest {
            req: "p1".into(),
            session: "ses_test".into(),
            title: "Exec".into(),
            detail: "cargo test".into(),
        },
    );
    let (_, text_with_perm) = render_size(&mut app, 140, 40);
    assert!(
        text_with_perm.contains("Exec"),
        "pending banner rendered with right_bus: {text_with_perm}"
    );
    assert!(
        text_with_perm.contains("Flow"),
        "Flow pane rendered alongside banner: {text_with_perm}"
    );
    assert!(
        text_with_perm.contains("Bus / Tasks"),
        "Bus pane rendered alongside banner: {text_with_perm}"
    );
}

#[test]
fn right_pane_split_flow_and_bus_under_threshold() {
    let mut app = sample_app(&["echo 1"]);
    app.layout = LayoutMode::Three;
    app.right_bus = true;

    // Height < 16 for right pane area (total terminal height 20 -> chunks[1].height is 14)
    let (_, short_text) = render_size(&mut app, 140, 20);
    assert!(short_text.contains("Flow"));
    assert!(!short_text.contains("Bus / Tasks"));

    // Height >= 16 for right pane area (total terminal height 24 -> chunks[1].height is 18)
    let (_, tall_text) = render_size(&mut app, 140, 24);
    assert!(tall_text.contains("Flow"));
    assert!(tall_text.contains("Bus / Tasks"));
}

#[test]
fn toggle_bus_flow_status_messages() {
    let mut app = sample_app(&[]);
    app.toggle_bus();
    assert_eq!(app.status, "Flow：顯示 (b 或 Esc 關閉)");
    assert!(app.bus_overlay);
    app.toggle_bus();
    assert_eq!(app.status, "Flow：隱藏");
    assert!(!app.bus_overlay);
}

#[test]
fn quota_spans_ignores_3p_windows() {
    let mut app = sample_app(&[]);
    let q = ertangle_core::Quota {
        agent: "antigravity".into(),
        warning: false,
        reached: false,
        updated_ms: 0,
        windows: vec![
            ertangle_core::QuotaWindow {
                name: "3p-7d".into(),
                used_percent: 90.0,
                resets_at_ms: None,
            },
            ertangle_core::QuotaWindow {
                name: "5h".into(),
                used_percent: 40.0,
                resets_at_ms: None,
            },
        ],
    };
    app.state.quotas.insert("antigravity".into(), q);
    let spans = crate::draw::quota_spans(&app);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!text.contains("3p-7d"), "3p window must be ignored: {text}");
    assert!(text.contains("5h"), "normal window must be shown: {text}");
}

#[test]
fn header_shows_flow_tab_and_highlights_when_active() {
    let mut app = sample_app(&[]);
    let text = render(&mut app);
    assert!(text.contains("Flow (F)"), "header shows Flow (F): {text}");

    app.view = CenterView::Flow;
    let text_active = render(&mut app);
    assert!(
        text_active.contains("[Flow (F)]"),
        "Flow (F) highlighted when active: {text_active}"
    );
}

#[test]
fn adaptive_three_column_expansion() {
    let mut app = sample_app(&["echo 1"]);
    app.scope_here = true;
    app.home_project = "/work/app".into();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.cwd = "/work/app".into();
    }
    // Add a second live session so live count >= 2.
    let mut s2 = app.state.sessions.get("ses_test").unwrap().clone();
    s2.id = "ses_test2".into();
    s2.cwd = "/work/app".into();
    s2.label = "worker2".into();
    s2.status = SessionStatus::Busy;
    app.state.sessions.insert("ses_test2".into(), s2);

    assert_eq!(app.layout, LayoutMode::Two);
    assert!(!app.layout_pinned);

    // >= 130 width with 2 live sessions adapts to Three (showing Flow).
    let (_, wide) = render_size(&mut app, 130, 40);
    assert!(
        squash(&wide).contains(&squash("Flow ·")),
        "wide terminal adapts to Three: {wide}"
    );

    // Narrow terminal (< 130) stays Two.
    let (_, narrow) = render_size(&mut app, 129, 40);
    assert!(
        !squash(&narrow).contains(&squash("Flow ·")),
        "narrow terminal stays Two: {narrow}"
    );

    // Wide terminal (>= 130) adapts to Three even with 1 live session unless pinned.
    app.state.sessions.get_mut("ses_test2").unwrap().status = SessionStatus::Exited;
    let (_, single) = render_size(&mut app, 130, 40);
    assert!(
        squash(&single).contains(&squash("Flow ·")),
        "wide terminal with 1 live session adapts to Three: {single}"
    );

    // Pinned layout stays Two even when wide with >= 2 live sessions.
    app.state.sessions.get_mut("ses_test2").unwrap().status = SessionStatus::Busy;
    app.set_layout(LayoutMode::Two);
    assert!(app.layout_pinned);
    let (_, pinned) = render_size(&mut app, 130, 40);
    assert!(
        !squash(&pinned).contains(&squash("Flow ·")),
        "pinned layout stays Two: {pinned}"
    );
}

#[test]
fn other_project_summary_rows_in_sidebar() {
    let mut app = sample_app(&["echo 1"]);
    app.home_project = "/work/alpha".into();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.cwd = "/work/alpha".into();
    }
    // Add a session in another project.
    let mut s_other = app.state.sessions.get("ses_test").unwrap().clone();
    s_other.id = "ses_other".into();
    s_other.cwd = "/work/beta".into();
    s_other.label = "other-worker".into();
    s_other.status = SessionStatus::Busy;
    app.state.sessions.insert("ses_other".into(), s_other);

    app.scope_here = true;
    let summaries = app.other_project_summaries();
    assert_eq!(summaries, vec![("beta".to_string(), 1, 1)]);

    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("Projects & Sessions"), "panel title: {text}");
    assert!(text.contains("▸ beta (1 live / 1)"), "summary row: {text}");
    assert!(
        squash(&text).contains(&squash("P 展開 · Space P 切換")),
        "hint row: {text}"
    );
    // Selection stays on the first session in the current project.
    assert_eq!(app.selected, 0);

    // When scope_here is false, other_project_summaries is empty.
    app.scope_here = false;
    assert!(app.other_project_summaries().is_empty());
}

#[test]
fn t88_submitting_prompt_to_exited_session_opens_confirm_picker() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    app.show_ended = true;
    let (mut w, mut peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Exited;
        s.native_id = Some("native_conv_1".into());
    }
    app.mode = Mode::Prompt;
    app.input = "hello world".into();
    app.input_cursor = "hello world".chars().count();

    // Submit prompt by pressing Enter
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();

    // Verify no Request::Prompt was sent to peer directly
    let mut byte = [0u8; 1];
    assert!(matches!(
        peer.read(&mut byte),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    ));

    // Verify picker is open with ConfirmResumePrompt
    assert_eq!(app.mode, Mode::Pick);
    let p = app.picker.as_ref().expect("picker must be open");
    assert!(matches!(
        &p.stage,
        crate::menu::PickStage::ConfirmResumePrompt {
            session,
            status: SessionStatus::Exited,
            resumable: true,
            text,
            ..
        } if session == "ses_test" && text == "hello world"
    ));
    assert_eq!(p.items.len(), 2);
    assert_eq!(p.items[0].1, "confirm");
    assert_eq!(p.items[1].1, "cancel");
}

#[test]
fn t88_confirming_resume_sends_resume_request() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    app.show_ended = true;
    let (mut w, mut peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Exited;
        s.native_id = Some("native_conv_1".into());
    }
    app.mode = Mode::Prompt;
    app.input = "resume with this prompt".into();
    app.input_cursor = app.input.chars().count();

    // Press Enter to open confirmation picker
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Pick);

    // Press Enter on the first option ("confirm")
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.picker.is_none());

    let mut buf = [0u8; 1024];
    let n = peer.read(&mut buf).unwrap();
    let msg = String::from_utf8_lossy(&buf[..n]);
    assert!(msg.contains("\"resume\""), "got: {msg}");
    assert!(msg.contains("resume with this prompt"), "got: {msg}");
    assert!(msg.contains("ses_test"), "got: {msg}");
}

#[test]
fn t88_confirming_resume_with_attachments_sends_resume_then_prompt() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    app.show_ended = true;
    let (mut w, mut peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Exited;
        s.native_id = Some("native_conv_1".into());
    }

    // Directly invoke start_confirm_resume_prompt with an attachment
    let att = ertangle_core::Attachment {
        name: "test.txt".into(),
        mime: "text/plain".into(),
        text: Some("hello attachment".into()),
        ..Default::default()
    };
    crate::menu::start_confirm_resume_prompt(
        &mut app,
        "ses_test".into(),
        "worker".into(),
        SessionStatus::Exited,
        true,
        "prompt text".into(),
        vec![att],
    );
    assert_eq!(app.mode, Mode::Pick);

    // Confirm
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.picker.is_none());

    let mut buf = [0u8; 2048];
    let n = peer.read(&mut buf).unwrap();
    let msg = String::from_utf8_lossy(&buf[..n]);
    // Should have Resume with prompt: None, followed by Prompt with attachments
    assert!(msg.contains("\"resume\""), "got: {msg}");
    assert!(msg.contains("\"prompt\""), "got: {msg}");
    assert!(msg.contains("prompt text"), "got: {msg}");
    assert!(msg.contains("test.txt"), "got: {msg}");
}

#[test]
fn t88_confirming_sleeping_sends_wake_and_prompt() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    let (mut w, mut peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Sleeping;
    }
    app.mode = Mode::Prompt;
    app.input = "wake up agent".into();
    app.input_cursor = app.input.chars().count();

    // Press Enter to open confirmation picker
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Pick);

    // Confirm
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);

    let mut buf = [0u8; 1024];
    let n = peer.read(&mut buf).unwrap();
    let msg = String::from_utf8_lossy(&buf[..n]);
    assert!(msg.contains("\"wake\""), "got: {msg}");
    assert!(msg.contains("\"prompt\""), "got: {msg}");
    assert!(msg.contains("wake up agent"), "got: {msg}");
}

#[test]
fn t88_cancelling_confirm_picker_restores_draft_and_prompt_mode() {
    let mut app = sample_app(&[]);
    app.show_ended = true;
    let (mut w, _peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Exited;
        s.native_id = Some("native_conv_1".into());
    }
    app.mode = Mode::Prompt;
    app.input = "important draft text".into();
    app.input_cursor = app.input.chars().count();

    // Enter to open picker
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Pick);
    assert!(app.input.is_empty(), "input was cleared when submitted");

    // Press Esc to cancel
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "important draft text");
    assert_eq!(app.input_cursor, "important draft text".chars().count());
    assert!(app.picker.is_none());

    // Enter again to open picker
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Pick);

    // Select second option ("cancel") via Down arrow then Enter
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    )
    .unwrap();
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "important draft text");
    assert_eq!(app.input_cursor, "important draft text".chars().count());
    assert!(app.picker.is_none());
}

#[test]
fn t88_non_resumable_exited_session_only_allows_cancel() {
    let mut app = sample_app(&[]);
    app.show_ended = true;
    let (mut w, _peer) = pair_writer();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.status = SessionStatus::Failed;
        s.native_id = None;
    }
    app.mode = Mode::Prompt;
    app.input = "text for failed non-resumable".into();
    app.input_cursor = app.input.chars().count();

    // Enter to open picker
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Pick);

    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].1, "cancel");

    // Enter on the only option cancels and restores draft
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "text for failed non-resumable");
    assert!(app.picker.is_none());
}

#[test]
fn t86_layout_cycling_and_auto_sync() {
    let mut app = sample_app(&["echo 1"]);
    let (mut w, _peer) = pair_writer();

    // On wide terminal (>= 130), adaptive layout renders Three while desired layout was Two.
    assert_eq!(app.layout, LayoutMode::Two);
    assert!(!app.layout_pinned);
    let _ = render_size(&mut app, 130, 40);
    assert_eq!(app.rendered_layout, LayoutMode::Three);

    // Pressing 'b' in adaptive 3-column mode toggles right_bus (not bus_overlay).
    assert!(app.right_bus);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('b'))).unwrap();
    assert!(!app.right_bus);
    handle_key(&mut app, &mut w, kev(KeyCode::Char('b'))).unwrap();
    assert!(app.right_bus);

    // Pressing 'L' cycles from rendered_layout (Three): Three.next() == Two.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('L'))).unwrap();
    assert_eq!(app.layout, LayoutMode::Two);
    assert!(app.layout_pinned);

    // Next 'L': Two.next() == Tabs.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('L'))).unwrap();
    assert_eq!(app.layout, LayoutMode::Tabs);

    // Next 'L': Tabs.next() == Three.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('L'))).unwrap();
    assert_eq!(app.layout, LayoutMode::Three);

    // :layout auto unpins layout and resets to Two.
    execute_command(&mut app, &mut w, "/layout auto").unwrap();
    assert!(!app.layout_pinned);
    assert_eq!(app.layout, LayoutMode::Two);

    // Re-rendering at wide width adapts to Three again.
    let _ = render_size(&mut app, 130, 40);
    assert_eq!(app.rendered_layout, LayoutMode::Three);
}

#[test]
fn t87_sidebar_keyboard_navigation_and_focus() {
    let mut app = sample_app(&["echo 1"]);
    let (mut w, _peer) = pair_writer();
    app.home_project = "/work/alpha".into();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.cwd = "/work/alpha".into();
    }
    // Add an ended session in /work/alpha.
    let mut s_ended = app.state.sessions.get("ses_test").unwrap().clone();
    s_ended.id = "ses_ended".into();
    s_ended.cwd = "/work/alpha".into();
    s_ended.label = "ended-worker".into();
    s_ended.status = SessionStatus::Exited;
    app.state.sessions.insert("ses_ended".into(), s_ended);

    // Add a session in another project /work/beta.
    let mut s_other = app.state.sessions.get("ses_test").unwrap().clone();
    s_other.id = "ses_other".into();
    s_other.cwd = "/work/beta".into();
    s_other.label = "other-worker".into();
    s_other.status = SessionStatus::Busy;
    app.state.sessions.insert("ses_other".into(), s_other);

    app.scope_here = true;
    app.show_ended = false;
    assert_eq!(app.visible_sessions().len(), 1);
    assert_eq!(app.ended_count(), 1);
    assert_eq!(app.other_project_roots().len(), 1);

    // Initially at session 0, sidebar_focus is None.
    assert_eq!(app.selected, 0);
    assert_eq!(app.sidebar_focus, None);

    // Moving up at top stays at session 0.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('k'))).unwrap();
    assert_eq!(app.selected, 0);
    assert_eq!(app.sidebar_focus, None);

    // Moving down past the last visible session enters EndedFold.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));

    // Moving down from EndedFold enters OtherProject(0).
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::OtherProject(0)));

    // Moving down past the last other project stays at OtherProject(0).
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::OtherProject(0)));

    // Moving up returns to EndedFold.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('k'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));

    // Moving up from EndedFold returns to last visible session.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('k'))).unwrap();
    assert_eq!(app.sidebar_focus, None);
    assert_eq!(app.selected, 0);

    // Esc clears sidebar_focus if active.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));
    handle_key(&mut app, &mut w, kev(KeyCode::Esc)).unwrap();
    assert_eq!(app.sidebar_focus, None);
}

#[test]
fn t87_sidebar_activation_and_project_switch() {
    let mut app = sample_app(&["echo 1"]);
    let (mut w, _peer) = pair_writer();
    app.home_project = "/work/alpha".into();
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.cwd = "/work/alpha".into();
    }
    let mut s_ended = app.state.sessions.get("ses_test").unwrap().clone();
    s_ended.id = "ses_ended".into();
    s_ended.cwd = "/work/alpha".into();
    s_ended.label = "ended-worker".into();
    s_ended.status = SessionStatus::Exited;
    app.state.sessions.insert("ses_ended".into(), s_ended);

    let mut s_other = app.state.sessions.get("ses_test").unwrap().clone();
    s_other.id = "ses_other".into();
    s_other.cwd = "/work/beta".into();
    s_other.label = "other-worker".into();
    s_other.status = SessionStatus::Busy;
    app.state.sessions.insert("ses_other".into(), s_other);

    app.scope_here = true;
    app.show_ended = false;

    // Navigate to EndedFold and activate with 'l' / Enter.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));
    handle_key(&mut app, &mut w, kev(KeyCode::Char('l'))).unwrap();
    assert!(app.show_ended);
    assert_eq!(app.sidebar_focus, None);
    assert_eq!(app.selected, 1, "focuses the first ended session");

    // The fold header sits above the expanded ended sessions: row 1 of the list, ended at row 2.
    assert_eq!(app.ended_fold_row(app.visible_sessions().len()), 1);
    assert_eq!(app.session_list_row(0, true), 0);
    assert_eq!(app.session_list_row(1, true), 2);

    // Navigate up onto EndedFold again and collapse with 'h'.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('k'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));
    // Down from the fold re-enters the first ended session below it.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!((app.sidebar_focus, app.selected), (None, 1));
    handle_key(&mut app, &mut w, kev(KeyCode::Char('k'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));
    handle_key(&mut app, &mut w, kev(KeyCode::Char('h'))).unwrap();
    assert!(!app.show_ended);

    // Navigate to OtherProject(0) and activate with Enter.
    handle_key(&mut app, &mut w, kev(KeyCode::Char('j'))).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::OtherProject(0)));
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.home_project, "/work/beta");
    assert_eq!(app.sidebar_focus, None);
    assert_eq!(app.selected, 0);
    assert_eq!(app.selected_id().as_deref(), Some("ses_other"));
}

#[test]
fn t87_sidebar_zero_live_sessions_navigation() {
    let mut app = sample_app(&["echo 1"]);
    let (mut w, _peer) = pair_writer();
    app.home_project = "/work/alpha".into();
    // Mark the only session in /work/alpha as Exited so live sessions count is 0.
    if let Some(s) = app.state.sessions.get_mut("ses_test") {
        s.cwd = "/work/alpha".into();
        s.status = SessionStatus::Exited;
    }
    // Add another project session.
    let mut s_other = app.state.sessions.get("ses_test").unwrap().clone();
    s_other.id = "ses_other".into();
    s_other.cwd = "/work/beta".into();
    s_other.label = "beta-worker".into();
    s_other.status = SessionStatus::Busy;
    app.state.sessions.insert("ses_other".into(), s_other);

    app.scope_here = true;
    app.show_ended = false;
    assert_eq!(app.visible_sessions().len(), 0);

    // Down or j navigates onto EndedFold even when live sessions is 0.
    handle_key(&mut app, &mut w, kev(KeyCode::Down)).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::EndedFold));

    handle_key(&mut app, &mut w, kev(KeyCode::Down)).unwrap();
    assert_eq!(app.sidebar_focus, Some(SidebarFocus::OtherProject(0)));

    // Activating switches project to /work/beta.
    handle_key(&mut app, &mut w, kev(KeyCode::Enter)).unwrap();
    assert_eq!(app.home_project, "/work/beta");
    assert_eq!(app.sidebar_focus, None);
    assert_eq!(app.selected, 0);
}

#[test]
fn resuming_ended_session_keeps_selection_and_shows_user_prompt() {
    let mut app = sample_app(&[]);
    app.show_ended = true;
    // Add an exited session ("ses_end_1") and another exited session ("ses_end_2") sorted before it.
    let mut s1 = ertangle_core::Session {
        id: "ses_end_1".into(),
        label: "ended-one".into(),
        kind: AgentKind::ClaudeCode,
        role: Role::Worker,
        parent: None,
        cwd: app.home_project.clone(),
        repo_root: None,
        branch: None,
        transport: "structured".into(),
        permission: PermissionPolicy::Auto,
        model: None,
        effort: None,
        status: SessionStatus::Exited,
        status_note: None,
        cost: ertangle_core::Cost::default(),
        context: None,
        budget_usd: None,
        over_budget: false,
        native_id: Some("nat_1".into()),
        handed_off_to: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
        created_ms: 1,
        ended_ms: Some(2),
    };
    let mut s2 = s1.clone();
    s2.id = "ses_end_2".into();
    s2.label = "ended-two".into();
    s2.created_ms = 2;
    s1.created_ms = 3;
    app.state.sessions.insert(s1.id.clone(), s1);
    app.state.sessions.insert(s2.id.clone(), s2);

    // Select `ses_end_1` (which is at the ended section).
    app.select_or_pend("ses_end_1");
    assert_eq!(app.selected_id().as_deref(), Some("ses_end_1"));

    // Simulate daemon emitting SessionStatus(Idle) + UserPrompt when resumed.
    let host = app.active_host().to_string();
    app.apply_host_frame(
        &host,
        ertangle_protocol::Frame::Record {
            record: Box::new(ertangle_core::Record {
                seq: 10,
                ts: 100,
                event: ertangle_core::Event::SessionStatus {
                    session: "ses_end_1".into(),
                    status: SessionStatus::Idle,
                },
            }),
        },
    );
    app.apply_host_frame(
        &host,
        ertangle_protocol::Frame::Record {
            record: Box::new(ertangle_core::Record {
                seq: 11,
                ts: 101,
                event: ertangle_core::Event::UserPrompt {
                    session: "ses_end_1".into(),
                    text: "resumed prompt text".into(),
                    from: None,
                },
            }),
        },
    );

    // Selection must remain on `ses_end_1` even after it moved from ended to live section.
    assert_eq!(app.selected_id().as_deref(), Some("ses_end_1"));
    let (_, rendered) = render_size(&mut app, 100, 30);
    assert!(
        rendered.contains("resumed prompt text"),
        "transcript should display the resumed prompt: {rendered}"
    );
}

#[test]
fn t94_shift_enter_and_ctrl_enter_newline() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "hello".into();
    app.input_cursor = 5;

    // Shift+Enter inserts newline in multiline mode (Prompt)
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.input_cursor, 6);

    // Ctrl+Enter also inserts newline
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Prompt);
    assert_eq!(app.input, "hello\n\n");
    assert_eq!(app.input_cursor, 7);

    // Ctrl+J fallback inserts newline
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input, "hello\n\n\n");
    assert_eq!(app.input_cursor, 8);
}

#[test]
fn t94_option_left_right_and_b_f() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "hello world 你好 測試".into();
    app.input_cursor = 0;

    // Option+Right word forward
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 5); // after "hello"

    // Alt+f word forward
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 11); // after "world"

    // Ctrl+Right word forward
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 14); // after "你好"

    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 17); // after "測試"

    // Option+Left word backward
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 15); // before "測試"

    // Alt+b word backward
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 12); // before "你好"

    // Ctrl+Left word backward
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 6); // before "world"

    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 0); // before "hello"

    // Ensure Alt+b/f did not insert literal characters
    assert_eq!(app.input, "hello world 你好 測試");
}

#[test]
fn t94_option_backspace_ctrl_w_and_option_delete_d() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "first second third".into();
    app.input_cursor = 18;

    // Option+Backspace deletes previous word
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input, "first second ");
    assert_eq!(app.input_cursor, 13);

    // Ctrl+W deletes previous word
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input, "first ");
    assert_eq!(app.input_cursor, 6);

    // Ctrl+Backspace deletes previous word
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input, "");
    assert_eq!(app.input_cursor, 0);

    // Forward deletion: Option+Delete and Option+d
    app.input = "alpha beta gamma".into();
    app.input_cursor = 0;

    // Option+Delete deletes forward word
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input, " beta gamma");
    assert_eq!(app.input_cursor, 0);

    // Alt+d deletes forward word
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT),
    )
    .unwrap();
    assert_eq!(app.input, " gamma");
    assert_eq!(app.input_cursor, 0);

    // Ctrl+Delete deletes forward word
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input, "");
    assert_eq!(app.input_cursor, 0);
}

#[test]
fn t94_ctrl_k_delete_to_line_end() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "hello world\nsecond line".into();
    app.input_cursor = 5;

    // Ctrl+K deletes from cursor to line end
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input, "hello\nsecond line");
    assert_eq!(app.input_cursor, 5);

    // Ctrl+K at line end deletes newline (joins lines)
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(app.input, "hellosecond line");
    assert_eq!(app.input_cursor, 5);
}

#[test]
fn t94_cmd_shortcuts_super_modifier() {
    let mut app = sample_app(&[]);
    let (mut w, _peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "line 1\nsecond line here\nline 3".into();
    app.input_cursor = 19;

    // Cmd+Left moves to line start
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 7); // start of "second line here"

    // Cmd+Right moves to line end
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER),
    )
    .unwrap();
    assert_eq!(app.input_cursor, 23); // end of "second line here"

    // Cmd+Backspace deletes to line start
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER),
    )
    .unwrap();
    assert_eq!(app.input, "line 1\n\nline 3");
    assert_eq!(app.input_cursor, 7);
}

#[test]
fn t96_no_session_plain_prompt_opens_picker_without_resumable() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    app.state.sessions.clear();
    app.state.archived_sessions.clear();
    let (mut w, mut peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "fix the build".into();
    app.input_cursor = app.input.chars().count();

    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();

    let mut byte = [0u8; 1];
    assert!(matches!(
        peer.read(&mut byte),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    ));

    assert_eq!(app.mode, Mode::Pick);
    let p = app.picker.as_ref().expect("picker must be open");
    assert!(matches!(
        &p.stage,
        crate::menu::PickStage::NoSessionPrompt {
            text,
            can_resume: false,
            ..
        } if text == "fix the build"
    ));
    assert_eq!(p.items.len(), 2);
    assert_eq!(p.items[0].1, "create");
    assert_eq!(p.items[1].1, "cancel");
}

#[test]
fn t96_no_session_with_resumable_resumes_and_sends_prompt() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    if let Some(mut s) = app.state.sessions.remove("ses_test") {
        s.status = SessionStatus::Exited;
        s.native_id = Some("native_conv_1".into());
        app.state.archived_sessions.insert("ses_test".into(), s);
    }
    let (mut w, mut peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "continue work".into();
    app.input_cursor = app.input.chars().count();

    // Open NoSessionPrompt
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    let p = app.picker.as_ref().expect("picker must be open");
    assert_eq!(p.items.len(), 3);
    assert_eq!(p.items[1].1, "resume");

    // Select "resume" (Down + Enter)
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    )
    .unwrap();
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();

    // Now in PickStage::Resume; press Enter to resume the session
    let p = app.picker.as_ref().expect("resume picker must be open");
    assert!(matches!(p.stage, crate::menu::PickStage::Resume));
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.picker.is_none());

    let mut buf = [0u8; 1024];
    let n = peer.read(&mut buf).unwrap();
    let msg = String::from_utf8_lossy(&buf[..n]);
    assert!(msg.contains("\"resume\""), "got: {msg}");
    assert!(msg.contains("continue work"), "got: {msg}");
    assert!(msg.contains("ses_test"), "got: {msg}");
}

#[test]
fn t96_no_session_create_carries_prompt_into_spawn_and_sends_once() {
    use std::io::Read;
    let mut app = sample_app(&[]);
    app.state.sessions.clear();
    let (mut w, mut peer) = pair_writer();
    app.mode = Mode::Prompt;
    app.input = "scaffold new api".into();
    app.input_cursor = app.input.chars().count();

    // Open NoSessionPrompt and choose "create"
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();

    // Finish agent/model picker into Mode::Spawn
    crate::menu::finish_picker(
        &mut app,
        &mut w,
        AgentKind::parse("claude").unwrap(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Spawn);
    assert_eq!(app.input, "scaffold new api");

    // Submit spawn
    handle_key(
        &mut app,
        &mut w,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);

    let mut buf = [0u8; 2048];
    let n = peer.read(&mut buf).unwrap();
    let msg = String::from_utf8_lossy(&buf[..n]);
    assert!(msg.contains("\"spawn\""), "got: {msg}");
    assert!(msg.contains("scaffold new api"), "got: {msg}");
}

#[test]
fn t96_no_session_cancel_and_esc_restore_draft() {
    for use_esc in [true, false] {
        let mut app = sample_app(&[]);
        app.state.sessions.clear();
        let (mut w, _peer) = pair_writer();
        app.mode = Mode::Prompt;
        app.input = "keep my draft".into();
        app.input_cursor = app.input.chars().count();

        handle_key(
            &mut app,
            &mut w,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.mode, Mode::Pick);

        if use_esc {
            handle_key(
                &mut app,
                &mut w,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            )
            .unwrap();
        } else {
            // Down to "cancel" then Enter
            handle_key(
                &mut app,
                &mut w,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            )
            .unwrap();
            handle_key(
                &mut app,
                &mut w,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .unwrap();
        }

        assert_eq!(app.mode, Mode::Prompt);
        assert!(app.picker.is_none());
        assert_eq!(app.input, "keep my draft");
        assert_eq!(app.input_cursor, "keep my draft".chars().count());
    }
}

#[test]
fn t96_no_session_routed_commands_do_not_open_picker() {
    for input in ["@coord hello", "#general hi", "/help"] {
        let mut app = sample_app(&[]);
        app.state.sessions.clear();
        let (mut w, _peer) = pair_writer();
        app.mode = Mode::Prompt;
        app.input = input.into();
        app.input_cursor = app.input.chars().count();

        handle_key(
            &mut app,
            &mut w,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .unwrap();
        assert_ne!(
            app.mode,
            Mode::Pick,
            "routed input {input} should not open picker"
        );
    }
}
