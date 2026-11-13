use super::*;
use ade_core::{Cost, Role, Session};
use ratatui::backend::TestBackend;
use std::collections::VecDeque;

fn sample_app(lines: &[&str]) -> App {
    let mut app = App::new();
    let s = Session {
        id: "ses_test".into(),
        kind: AgentKind::OpenCode,
        label: "worker".into(),
        cwd: "/tmp".into(),
        branch: Some("ade/x".into()),
        role: Role::Worker,
        status: SessionStatus::Busy,
        model: None,
        cost: Cost::default(),
        parent: None,
        created_ms: 1,
        transport: "acp".into(),
        status_note: None,
        repo_root: None,
        permission: Default::default(),
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
    assert!(rendered.contains("md"), "mode indicator missing");

    app.raw_md = true;
    let raw = render(&mut app);
    assert!(raw.contains("**bold**"), "raw mode should show markup");
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
    app.state.apply(&ade_core::Record {
        seq: 1,
        ts: 1,
        event: ade_core::Event::UserPrompt {
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
    let mut app = App::new();
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
        ade_core::PermissionRequest {
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
    app.state.messages.push(ade_core::BusMessage {
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
        .push_back("▸ mcp__ade__ade_peers: {\"timeout_ms\":1}".to_string());
    let (_, text) = render_size(&mut app, 120, 40);
    assert!(text.contains("mcp__ade__ade_peers"), "{text}");
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
    let d = std::env::temp_dir().join(format!("ade-att-{}", ade_core::new_id("t")));
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
    std::fs::write(&big, vec![0u8; (ade_core::MAX_IMAGE_BYTES + 1) as usize]).unwrap();
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
