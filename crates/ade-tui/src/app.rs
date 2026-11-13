//! TUI 狀態模型、版面模式與畫面快取。

use std::time::Duration;

use ade_core::{AgentKind, PermissionPolicy, SessionId, State, WorktreeInfo};
use ratatui::layout::Rect;
use ratatui::text::Line;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum Mode {
    Normal,
    Prompt,
    Spawn,
    Message,
    /// M3：對選取的 diff 行下審查註解並送回該 agent。
    Review,
    /// M4：命令面板（`/` 或 `Ctrl-P`）。
    Command,
    /// M4：快速跳轉任何 session（`Ctrl-K`）。
    Jump,
    /// Space 選單（which-key）。
    Menu,
    /// 清單選擇器（spawn 的 agent／model）。
    Pick,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum CenterView {
    Transcript,
    Diff,
    Worktrees,
}

/// 主區版面模式。
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum LayoutMode {
    /// Sessions／中央／Bus 三欄。
    Three,
    /// Sessions／中央兩欄，隱藏 Bus。
    Two,
    /// 上方 session 分頁列，中央佔滿整個寬度。
    Tabs,
}

impl LayoutMode {
    pub(crate) fn next(self) -> Self {
        match self {
            LayoutMode::Three => LayoutMode::Two,
            LayoutMode::Two => LayoutMode::Tabs,
            LayoutMode::Tabs => LayoutMode::Three,
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            LayoutMode::Three => "three",
            LayoutMode::Two => "two",
            LayoutMode::Tabs => "tabs",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "three" | "3" => Some(LayoutMode::Three),
            "two" | "2" => Some(LayoutMode::Two),
            "tabs" | "tab" => Some(LayoutMode::Tabs),
            _ => None,
        }
    }
}

/// 窄終端自動降級：寬度不足以容納所選版面時改用較省空間的版面。
/// `Tabs` 是使用者的明確選擇，不會被改動。
pub(crate) fn effective_layout(mode: LayoutMode, width: u16) -> LayoutMode {
    match mode {
        LayoutMode::Three if width >= 84 => LayoutMode::Three,
        LayoutMode::Three | LayoutMode::Two if width >= 64 => LayoutMode::Two,
        LayoutMode::Three | LayoutMode::Two => LayoutMode::Tabs,
        LayoutMode::Tabs => LayoutMode::Tabs,
    }
}

/// 串流期間逐字稿最短重算間隔；其餘變動（換 session／寬度／模式）立即重算。
pub(crate) const MD_REBUILD_INTERVAL: Duration = Duration::from_millis(120);

/// 逐字稿渲染快取：只有來源／寬度／模式改變時才重算 Markdown。
pub(crate) struct MdCache {
    pub(crate) session: SessionId,
    /// 該 session 逐字稿的輕量指紋（行數、首行長、末行長）；其他 session 或
    /// 非逐字稿事件造成的 `state.seq` 變動不會讓快取失效。
    pub(crate) stamp: (usize, usize, usize),
    pub(crate) raw: bool,
    pub(crate) width: u16,
    pub(crate) lines: Vec<Line<'static>>,
}

pub(crate) struct App {
    pub(crate) state: State,
    pub(crate) selected: usize,
    pub(crate) view: CenterView,
    pub(crate) mode: Mode,
    pub(crate) input: String,
    /// 輸入游標位置（以 Unicode 字元計，0 = 最前面）。
    pub(crate) input_cursor: usize,
    pub(crate) status: String,
    pub(crate) spawn_worktree: bool,
    /// 建立 session 的權限；`None` 依 config.toml。
    pub(crate) spawn_permission: Option<PermissionPolicy>,
    pub(crate) spawn_bypass_armed: bool,
    /// 選擇器選好的 (agent, model)；Spawn 模式的輸入就只是 prompt。
    pub(crate) spawn_target: Option<(AgentKind, Option<String>)>,
    pub(crate) spawn_effort: Option<String>,
    /// `@` 檔案建議：上次的查詢與選取索引。
    pub(crate) at_sel: (String, usize),
    /// 選到的 model 有哪些 effort 變體（Antigravity：effort 是 model id 的尾巴）。
    pub(crate) spawn_variants: Vec<String>,
    /// Space 選單目前走到的路徑。
    pub(crate) menu_path: Vec<char>,
    pub(crate) picker: Option<crate::menu::Picker>,
    /// M3：目前顯示的 worktree diff 內容與游標行。
    pub(crate) diff_session: Option<SessionId>,
    pub(crate) diff_lines: Vec<String>,
    pub(crate) diff_cursor: usize,
    /// 每行對應的（舊檔行號, 新檔行號）邊欄。
    pub(crate) diff_gutter: Vec<(Option<u64>, Option<u64>)>,
    pub(crate) diff_added: usize,
    pub(crate) diff_removed: usize,
    /// M3：受管 worktrees 快照。
    pub(crate) worktrees: Vec<WorktreeInfo>,
    /// 逐字稿捲動位置（渲染後行索引）；`None` = 跟隨最新輸出。
    pub(crate) transcript_scroll: Option<usize>,
    /// 最近一次繪製時可見的行數與總行數（供翻頁與捲軸使用）。
    pub(crate) transcript_viewport: usize,
    pub(crate) transcript_total: usize,
    /// 逐字稿是否以原始文字（不做 Markdown 渲染）顯示。
    pub(crate) raw_md: bool,
    /// 是否顯示 agent 的思考行（`💭`）。預設隱藏，避免長篇思考拖慢渲染。
    pub(crate) show_thoughts: bool,
    pub(crate) md_cache: Option<MdCache>,
    /// 上次重算逐字稿的時間；串流期間用來節流（見 `MD_REBUILD_INTERVAL`）。
    pub(crate) last_rebuild: std::time::Instant,
    /// 因節流而延後的重算，下一輪迴圈需補畫。
    pub(crate) rebuild_pending: bool,
    /// 最近一次繪製時 Sessions 清單的區域（供滑鼠點選）。
    pub(crate) sessions_area: Rect,
    /// Sessions 清單目前捲動的起始索引（滑鼠點選需加此偏移）。
    pub(crate) sessions_offset: usize,
    /// 最近一次繪製時逐字稿的區域、捲動偏移與可用寬度（供滑鼠選取）。
    pub(crate) transcript_area: Rect,
    pub(crate) transcript_offset: usize,
    pub(crate) transcript_inner_w: usize,
    /// 滑鼠拖曳選取範圍：((起始行, 起始欄), (目前行, 目前欄))，
    /// 行是渲染後的絕對行索引，欄是顯示欄位。
    pub(crate) selection: Option<((usize, usize), (usize, usize))>,
    /// 是否擷取滑鼠（預設開啟，支援應用內拖曳選取；`F2` 關閉以使用終端原生選取）。
    pub(crate) mouse_capture: bool,
    /// 使用者選擇的版面模式（僅執行期；T11 的 config.toml 完成後再持久化）。
    pub(crate) layout: LayoutMode,
    /// Bus 欄被隱藏時，是否以浮層暫時顯示。
    pub(crate) bus_overlay: bool,
    /// 最近一次繪製時 Bus 是否可見，以及當時已看過的訊息數（供未讀計數）。
    pub(crate) bus_visible: bool,
    pub(crate) bus_seen: usize,
    /// 分頁模式下每個分頁的區域與對應 session 索引（供滑鼠點選）。
    pub(crate) tab_areas: Vec<(Rect, usize)>,
    pub(crate) should_quit: bool,
}

impl App {
    pub(crate) fn new() -> Self {
        let global_dir = ade_daemon::config_dir();
        let cwd = std::env::current_dir().unwrap_or_default();
        let cfg = ade_daemon::config::Config::load(&global_dir, &cwd);
        let layout = cfg
            .tui
            .layout
            .as_deref()
            .and_then(LayoutMode::parse)
            .unwrap_or(LayoutMode::Three);
        App {
            state: State::new(),
            selected: 0,
            view: CenterView::Transcript,
            mode: Mode::Normal,
            input: String::new(),
            input_cursor: 0,
            status: "已連線 daemon".into(),
            spawn_worktree: false,
            spawn_permission: None,
            spawn_bypass_armed: false,
            spawn_target: None,
            spawn_effort: None,
            at_sel: (String::new(), 0),
            spawn_variants: Vec::new(),
            menu_path: Vec::new(),
            picker: None,
            diff_session: None,
            diff_lines: Vec::new(),
            diff_cursor: 0,
            diff_gutter: Vec::new(),
            diff_added: 0,
            diff_removed: 0,
            worktrees: Vec::new(),
            transcript_scroll: None,
            transcript_viewport: 0,
            transcript_total: 0,
            raw_md: false,
            show_thoughts: false,
            md_cache: None,
            last_rebuild: std::time::Instant::now(),
            rebuild_pending: false,
            sessions_area: Rect::default(),
            sessions_offset: 0,
            transcript_area: Rect::default(),
            transcript_offset: 0,
            transcript_inner_w: 0,
            selection: None,
            mouse_capture: true,
            layout,
            bus_overlay: false,
            bus_visible: true,
            bus_seen: 0,
            tab_areas: Vec::new(),
            should_quit: false,
        }
    }

    pub(crate) fn session_ids(&self) -> Vec<SessionId> {
        self.state
            .sessions_sorted()
            .iter()
            .map(|s| s.id.clone())
            .collect()
    }

    /// 選取中 session 的工作目錄（檔案搜尋與 `@` 補全的根）。
    pub(crate) fn selected_cwd(&self) -> Option<String> {
        let id = self.selected_id()?;
        self.state.sessions.get(&id).map(|s| s.cwd.clone())
    }

    pub(crate) fn search_root(&self) -> std::path::PathBuf {
        crate::attach::search_root(self.selected_cwd().as_deref())
    }

    pub(crate) fn selected_id(&self) -> Option<SessionId> {
        self.session_ids().get(self.selected).cloned()
    }

    /// 回傳目前選取 session 尚未回覆的審批請求 id。
    pub(crate) fn selected_permission(&self) -> Option<String> {
        let id = self.selected_id()?;
        self.state
            .pending_permissions
            .values()
            .find(|p| p.session == id)
            .map(|p| p.req.clone())
    }

    pub(crate) fn unread_bus(&self) -> usize {
        if self.bus_visible {
            0
        } else {
            self.state.messages.len().saturating_sub(self.bus_seen)
        }
    }

    pub(crate) fn cycle_layout(&mut self) {
        self.set_layout(self.layout.next());
    }

    pub(crate) fn set_layout(&mut self, mode: LayoutMode) {
        self.layout = mode;
        self.selection = None;
        self.status = format!("版面：{}", mode.id());
    }

    pub(crate) fn clamp_selection(&mut self) {
        let n = self.session_ids().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// 取得目前 diff 游標所在行的位置描述（`檔案 :: 行內容`）。
    pub(crate) fn current_diff_location(&self) -> String {
        if self.diff_lines.is_empty() {
            return "diff".into();
        }
        let idx = self.diff_cursor.min(self.diff_lines.len() - 1);
        let mut current_file = "unknown";
        for l in &self.diff_lines[..=idx] {
            if let Some(rest) = l.strip_prefix("+++ b/") {
                current_file = rest.trim();
            } else if let Some(rest) = l.strip_prefix("diff --git ") {
                current_file = rest.trim();
            }
        }
        let line_text = self.diff_lines[idx].trim();
        format!("{current_file} (L{}: {})", idx + 1, truncate(line_text, 48))
    }
}

pub(crate) fn main_pane_text(app: &App) -> String {
    match app.view {
        CenterView::Transcript => app
            .selected_id()
            .and_then(|id| app.state.transcripts.get(&id))
            .map(|buf| {
                buf.iter()
                    .filter(|l| app.show_thoughts || !l.starts_with("💭 "))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        CenterView::Diff => app.diff_lines.join("\n"),
        CenterView::Worktrees => app
            .worktrees
            .iter()
            .map(|w| {
                format!(
                    "{} @{} {} {} ({})",
                    w.label,
                    w.branch,
                    if w.live { "live" } else { "exited" },
                    w.summary,
                    w.path
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// 切換是否顯示 agent 的思考行（`💭`）。
pub(crate) fn toggle_thoughts(app: &mut App) {
    app.show_thoughts = !app.show_thoughts;
    app.md_cache = None;
    app.selection = None;
    app.status = if app.show_thoughts {
        "思考顯示：開（顯示 💭 行）".into()
    } else {
        "思考顯示：關（已隱藏 💭 行，按 T 顯示）".into()
    };
}

/// 逐字稿翻頁捲動：`dir < 0` 往上、`dir > 0` 往下；到底時恢復跟隨最新。
pub(crate) fn scroll_transcript(app: &mut App, dir: i32) {
    let page = app.transcript_viewport.max(1);
    let max_off = app.transcript_total.saturating_sub(app.transcript_viewport);
    let cur = match app.transcript_scroll {
        Some(o) => o.min(max_off),
        None => max_off,
    };
    let next = if dir < 0 {
        cur.saturating_sub(page)
    } else {
        (cur + page).min(max_off)
    };
    if next >= max_off {
        app.transcript_scroll = None;
        app.status = "逐字稿：跟隨最新".into();
    } else {
        app.transcript_scroll = Some(next);
        app.status = format!("逐字稿：第 {next} 行（End 回到最新）");
    }
}

/// 以「行」為單位捲動（滑鼠滾輪用），到底時恢復跟隨最新。
pub(crate) fn scroll_transcript_lines(app: &mut App, delta: i32) {
    let max_off = app.transcript_total.saturating_sub(app.transcript_viewport);
    let cur = app.transcript_scroll.unwrap_or(max_off).min(max_off);
    let next = if delta < 0 {
        cur.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (cur + delta as usize).min(max_off)
    };
    if next >= max_off {
        app.transcript_scroll = None;
    } else {
        app.transcript_scroll = Some(next);
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub(crate) fn short(id: &str) -> String {
    id.rsplit('_')
        .next()
        .unwrap_or(id)
        .chars()
        .take(6)
        .collect()
}
