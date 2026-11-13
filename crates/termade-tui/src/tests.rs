use super::*;
use ratatui::backend::TestBackend;
use std::collections::VecDeque;
use termade_core::{Cost, Role, Session};

fn sample_app(lines: &[&str]) -> App {
    let mut app = test_app();
    let s = Session {
        id: "ses_test".into(),
        kind: AgentKind::OpenCode,
        label: "worker".into(),
        cwd: "/tmp".into(),
        branch: Some("termade/x".into()),
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
    app.state.apply(&termade_core::Record {
        seq: 1,
        ts: 1,
        event: termade_core::Event::UserPrompt {
            session: "ses_test".into(),
            text: "fix the tests\nsecond line 中文".into(),
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

    let lines = build_transcript_lines(&app, &id, 80);
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
    let lines = build_transcript_lines(&app, &id, 80);
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
        stamp: (0, 0, 0),
        raw: false,
        width: 80,
        lines,
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

    // 反向拖曳（由下往上）也要正常。
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

fn pair_writer() -> (ConnStream, std::os::unix::net::UnixStream) {
    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
    b.set_nonblocking(true).unwrap();
    (ConnStream::Unix(a), b)
}

fn add_pending(app: &mut App) {
    app.state.pending_permissions.insert(
        "r1".into(),
        termade_core::PermissionRequest {
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
    let (_, three) = render_size(&mut app, 120, 40);
    assert!(three.contains("Sessions") && three.contains("Bus / Tasks"));

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
    assert_eq!(app.layout, LayoutMode::Two);
    execute_command(&mut app, &mut w, "/layout tabs").unwrap();
    assert_eq!(app.layout, LayoutMode::Tabs);
    execute_command(&mut app, &mut w, "/layout three").unwrap();
    assert_eq!(app.layout, LayoutMode::Three);

    app.layout = LayoutMode::Two;
    let _ = render_size(&mut app, 120, 40);
    app.state.messages.push(termade_core::BusMessage {
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
        .push_back("▸ mcp__termade__termade_peers: {\"timeout_ms\":1}".to_string());
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("mcp__termade__termade_peers"), "{text}");
    assert!(text.contains("timeout_ms"));
}

#[test]
fn space_menu_tree_has_session_and_git_groups() {
    let (_, root) = crate::menu::menu_items(&[]).unwrap();
    assert!(root.iter().any(|i| i.key == 's') && root.iter().any(|i| i.key == 'g'));
    let (_, s) = crate::menu::menu_items(&['s']).unwrap();
    assert!(s.iter().any(|i| i.key == 'n') && s.iter().any(|i| i.key == 'x'));
    assert!(crate::menu::menu_items(&['z']).is_none());
}

#[test]
fn picker_filters_and_offers_default_model() {
    let choices = crate::menu::model_choices(&AgentKind::ClaudeCode);
    assert_eq!(choices[0].1, "");
    assert!(choices.iter().any(|(_, v)| v == "opus"));
    let p = crate::menu::Picker {
        stage: crate::menu::PickStage::Agent,
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
    let m = crate::menu::parse_models(out);
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
    assert!(crate::commands::command_hints("zzz").is_empty());
}

#[test]
fn parses_codex_model_catalog_visible_only() {
    let out = r#"{"models":[{"slug":"a","display_name":"A","visibility":"list"},{"slug":"b","display_name":"B","visibility":"hide"}]}"#;
    let m = crate::menu::parse_codex_models(out);
    assert_eq!(m, vec![("a  (A)".to_string(), "a".to_string())]);
    assert!(crate::menu::parse_codex_models("not json").is_empty());
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

// ---- 附件（@檔案／圖片）----

fn temp_dir_with(files: &[(&str, &[u8])]) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("termade-att-{}", termade_core::new_id("t")));
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
        vec![0u8; (termade_core::MAX_IMAGE_BYTES + 1) as usize],
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
    // 只打檔名也能附上（唯一符合）
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
    let folded = crate::app::visible_transcript(src.iter(), false, false);
    assert_eq!(folded.len(), 5, "{folded:?}");
    assert!(folded[1].contains("工具 ×2"), "{folded:?}");
    assert!(folded.iter().any(|l| l.contains("✗ exit 1")));
    let full = crate::app::visible_transcript(src.iter(), false, true);
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
    let dir = std::env::temp_dir().join(format!("termade-hist-{}", std::process::id()));
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

    // 模擬 daemon 廣播 QueueChanged，確認 State.queues 保存原始輸入文字
    app.state.apply(&termade_core::Record {
        seq: 10,
        ts: 10,
        event: termade_core::Event::QueueChanged {
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
    let req1: Request = termade_protocol::decode_line(&line1).unwrap();
    let req2: Request = termade_protocol::decode_line(&line2).unwrap();
    let req3: Request = termade_protocol::decode_line(&line3).unwrap();
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
    let mk = |id: &str, cwd: &str, root: Option<&str>| termade_core::Session {
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
fn all_scope_shows_project_chip_and_groups_by_project() {
    let mut app = sample_app(&["hi"]);
    app.home_project = "/work/alpha".into();
    app.state.sessions.get_mut("ses_test").unwrap().cwd = "/work/beta".into();
    app.scope_here = false;
    let (_, text) = render_size(&mut app, 120, 40);
    let squashed: String = text.split_whitespace().collect();
    assert!(squashed.contains("beta"), "{text}");
    assert!(squashed.contains("全部專案"), "{text}");
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
    status: termade_core::TaskStatus,
    assignee: Option<&str>,
    deps: &[&str],
) {
    app.state.tasks.insert(
        id.into(),
        termade_core::Task {
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

fn key(app: &mut App, w: &mut ConnStream, code: KeyCode) {
    handle_key(app, w, KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
}

#[test]
fn tasks_view_groups_by_status_in_every_layout() {
    use termade_core::TaskStatus::*;
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
        assert!(text.contains("ccc333"), "{layout:?}: 缺依賴");
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
        termade_core::TaskStatus::Todo,
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
    use termade_core::TaskStatus::*;
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
        termade_core::TaskStatus::Todo,
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
    use termade_core::TaskStatus::Todo;
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

fn type_keys(app: &mut App, w: &mut ConnStream, text: &str) {
    for c in text.chars() {
        handle_key(app, w, kev(KeyCode::Char(c))).unwrap();
    }
}

/// 60 行逐字稿（畫面一次放不下），第 5、20、41 行含 `needle`，第 30 行含 `Needle`。
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
    apply_frame(
        &mut app,
        Frame::Diff {
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
    // Tab 補全表與 Space 選單都找得到，且 session 層的快捷鍵不重複。
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
    let req: Request = termade_protocol::decode_line(line.trim()).unwrap();
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
