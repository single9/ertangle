//! TUI 狀態模型、版面模式與畫面快取。

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tr::tr;

use ratatui::layout::Rect;
use ratatui::text::Line;
use termade_core::{
    now_millis, AgentKind, BusMessage, PermissionPolicy, Role, SessionId, State, Target,
    WorktreeInfo,
};
use termade_protocol::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::diff::{build_gutter, diff_stat, parse_files, rebuild_visible_diff, DiffFile};

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum Mode {
    Normal,
    Prompt,
    Spawn,
    Message,
    /// M3：對選取的 diff 行下審查註解並送回該 agent。
    Review,
    /// M4：命令面板 (`/` 或 `Ctrl-P`)。
    Command,
    /// M4：快速跳轉任何 session (`Ctrl-K`)。
    Jump,
    /// Space 選單 (which-key)。
    Menu,
    /// 說明面板。
    Help,
    /// 清單選擇器 (spawn 的 agent／model)。
    Pick,
    /// 在逐字稿或 diff 裡搜尋 (`f` 或 `:find`)。
    Search,
    /// 設定面板 (`:settings`)。
    Settings,
    /// Web 版 QR Code 彈窗。
    Qr,
    /// `:quota` 額度浮層。
    Quota,
    /// 背景處理程序與 Subagent 監控浮層 (`:ps` 或 `Space s p`)。
    Ps,
}

#[derive(Clone, Debug)]
pub(crate) struct QrModal {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) qr_lines: Vec<String>,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum CenterView {
    Transcript,
    Diff,
    Worktrees,
    /// T18：依狀態分組的任務清單。
    Tasks,
    /// 一次顯示最多 4 個 session 的迷你逐字稿。
    Grid,
}

/// 主區版面模式。
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum LayoutMode {
    /// Sessions／中央／Project 三欄 (Bus 可用 `:bus` 暫時換進右欄)。
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

/// 窄終端機自動降級：寬度不足以容納所選版面時改用較省空間的版面。
/// `Tabs` 是使用者的明確選擇，不會被改動。
pub(crate) fn effective_layout(mode: LayoutMode, width: u16) -> LayoutMode {
    match mode {
        LayoutMode::Three if width >= 84 => LayoutMode::Three,
        LayoutMode::Three | LayoutMode::Two if width >= 64 => LayoutMode::Two,
        LayoutMode::Three | LayoutMode::Two => LayoutMode::Tabs,
        LayoutMode::Tabs => LayoutMode::Tabs,
    }
}

/// 串流期間逐字稿最短重算間隔；其餘變動 (換 session／寬度／模式)立即重算。
pub(crate) const MD_REBUILD_INTERVAL: Duration = Duration::from_millis(120);

/// 「請先按 i 或 Enter」提示的顯示時間。
pub(crate) const INPUT_NUDGE_DURATION: Duration = Duration::from_millis(1800);

/// 逐字稿指紋：串流增量會改變末行或行數，滿額截斷會改變首行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct TranscriptStamp {
    pub(crate) len: usize,
    pub(crate) front_len: usize,
    pub(crate) front_hash: u64,
    pub(crate) back_len: usize,
    pub(crate) back_hash: u64,
}

/// 逐字稿渲染快取：包含已渲染之穩定前綴與可動態重算之尾端。
pub(crate) struct MdCache {
    pub(crate) session: SessionId,
    pub(crate) stamp: TranscriptStamp,
    pub(crate) raw: bool,
    pub(crate) width: u16,
    pub(crate) show_tools: bool,
    pub(crate) show_thoughts: bool,
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) links: Vec<crate::markdown::LinkTarget>,
    /// 穩定前綴所涵蓋的原始逐字稿行數。
    pub(crate) prefix_raw_len: usize,
    /// 穩定前綴產生的渲染行數。
    pub(crate) prefix_lines_count: usize,
    /// 穩定前綴產生的連結數。
    pub(crate) prefix_links_count: usize,
    /// 穩定前綴首行與末行的長度與 hash (用以偵測滿額截斷或中間修改)。
    pub(crate) prefix_front_hash: u64,
    pub(crate) prefix_front_len: usize,
    pub(crate) prefix_last_hash: u64,
    pub(crate) prefix_last_len: usize,
}

/// 單一主機的狀態與資料快照。
#[derive(Debug, Clone)]
pub(crate) struct HostData {
    #[allow(dead_code)]
    pub(crate) name: String,
    pub(crate) state: State,
    pub(crate) status: crate::hosts::HostStatus,
    pub(crate) daemon_epoch: Option<String>,
    pub(crate) diff_session: Option<SessionId>,
    pub(crate) diff_raw_lines: Vec<String>,
    pub(crate) diff_files: Vec<DiffFile>,
    pub(crate) diff_collapsed: BTreeSet<String>,
    pub(crate) diff_lines: Vec<String>,
    pub(crate) diff_cursor: usize,
    pub(crate) diff_gutter: Vec<(Option<u64>, Option<u64>)>,
    pub(crate) diff_added: usize,
    pub(crate) diff_removed: usize,
    pub(crate) worktrees: Vec<WorktreeInfo>,
}

impl HostData {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            state: State::default(),
            status: crate::hosts::HostStatus::Disconnected,
            daemon_epoch: None,
            diff_session: None,
            diff_raw_lines: Vec::new(),
            diff_files: Vec::new(),
            diff_collapsed: BTreeSet::new(),
            diff_lines: Vec::new(),
            diff_cursor: 0,
            diff_gutter: Vec::new(),
            diff_added: 0,
            diff_removed: 0,
            worktrees: Vec::new(),
        }
    }
}

pub(crate) struct App {
    pub(crate) state: State,
    /// 多主機狀態快照清單 (以 host name 為鍵)。
    pub(crate) hosts: BTreeMap<String, HostData>,
    /// 目前活躍／聚焦的主機名稱 (預設 "local")。
    pub(crate) active_host: String,
    /// 主機篩選器：`None` 代表顯示全部主機，`Some(h)` 代表僅顯示該主機。
    pub(crate) host_filter: Option<String>,
    pub(crate) selected: usize,
    pub(crate) view: CenterView,
    pub(crate) mode: Mode,
    pub(crate) input: String,
    /// 輸入游標位置 (以 Unicode 字元計，0 = 最前面)。
    pub(crate) input_cursor: usize,
    pub(crate) status: String,
    pub(crate) spawn_worktree: bool,
    /// 建立 session 的角色 (worker / coordinator / reviewer)；預設為 worker。
    pub(crate) spawn_role: Role,
    /// 建立 session 的權限；`None` 依 config.toml。
    pub(crate) spawn_permission: Option<PermissionPolicy>,
    pub(crate) spawn_bypass_armed: bool,
    /// 選擇器選好的 (agent, model)；Spawn 模式的輸入就只是 prompt。
    pub(crate) spawn_target: Option<(AgentKind, Option<String>)>,
    pub(crate) spawn_effort: Option<String>,
    pub(crate) help_scroll: usize,
    pub(crate) settings: Option<crate::settings::Settings>,
    /// 選擇器用於交接時，來源 session。
    pub(crate) handoff_from: Option<SessionId>,
    /// `@` 檔案建議：上次的查詢與選取索引。
    pub(crate) at_sel: (String, usize),
    /// 選到的 model 有哪些 effort 變體 (Antigravity：effort 是 model id 的尾巴)。
    pub(crate) spawn_variants: Vec<String>,
    /// Space 選單目前走到的路徑。
    pub(crate) menu_path: Vec<char>,
    pub(crate) picker: Option<crate::menu::Picker>,
    /// M3：目前顯示的 worktree diff 內容與游標行。
    pub(crate) diff_session: Option<SessionId>,
    pub(crate) diff_raw_lines: Vec<String>,
    pub(crate) diff_files: Vec<DiffFile>,
    pub(crate) diff_collapsed: BTreeSet<String>,
    pub(crate) diff_discard_armed: Option<(SessionId, String, Option<String>)>,
    pub(crate) diff_lines: Vec<String>,
    pub(crate) diff_cursor: usize,
    /// 每行對應的 (舊檔行號, 新檔行號)邊欄。
    pub(crate) diff_gutter: Vec<(Option<u64>, Option<u64>)>,
    pub(crate) diff_added: usize,
    pub(crate) diff_removed: usize,
    /// M3：受管 worktrees 快照。
    pub(crate) worktrees: Vec<WorktreeInfo>,
    /// 逐字稿捲動位置 (渲染後行索引)；`None` = 跟隨最新輸出。
    pub(crate) transcript_scroll: Option<usize>,
    /// 最近一次繪製時可見的行數與總行數 (供翻頁與捲軸使用)。
    pub(crate) transcript_viewport: usize,
    pub(crate) transcript_total: usize,
    /// 逐字稿是否以原始文字 (不做 Markdown 渲染)顯示。
    pub(crate) raw_md: bool,
    /// 是否顯示 agent 的思考行 (`💭`)。預設隱藏，避免長篇思考拖慢渲染。
    pub(crate) show_thoughts: bool,
    /// 展開每一筆工具呼叫；預設把連續的工具行摺成一行。
    pub(crate) show_tools: bool,
    /// 與 daemon 的連線已中斷 (例如別的 TUI 下了 shutdown)。
    pub(crate) disconnected: bool,
    /// daemon 啟動世代 id (epoch)，斷線重連時比對用。
    pub(crate) daemon_epoch: Option<String>,
    /// TUI 啟動所在的專案根目錄 (git 頂層，否則 cwd)。
    pub(crate) home_project: String,
    /// 只列出屬於 `home_project` 的 session。
    pub(crate) scope_here: bool,
    /// 輸入框內寬 (由繪製更新)，供上下移動游標換算顯示列。
    pub(crate) input_inner_w: usize,
    pub(crate) prompt_history: crate::history::History,
    pub(crate) command_history: crate::history::History,
    pub(crate) md_cache: Option<MdCache>,
    /// 上次重算逐字稿的時間；串流期間用來節流 (見 `MD_REBUILD_INTERVAL`)。
    pub(crate) last_rebuild: std::time::Instant,
    /// 因節流而延後的重算，下一輪迴圈需補畫。
    pub(crate) rebuild_pending: bool,
    /// 最近一次繪製時輸入框的區域 (供滑鼠點選與選取複製)。
    pub(crate) input_area: Rect,
    /// 輸入框內的滑鼠選取範圍 (以字元索引計的 `(錨點, 目前位置)`，含頭尾之間的字元)。
    pub(crate) input_sel: Option<(usize, usize)>,
    /// `Normal` 模式誤打字時記下的時間；用來提示要先進輸入模式。
    pub(crate) input_nudge: Option<std::time::Instant>,
    /// 最近一次繪製時 Sessions 清單的區域 (供滑鼠點選)。
    pub(crate) sessions_area: Rect,
    /// Sessions 清單目前捲動的起始索引 (滑鼠點選需加此偏移)。
    pub(crate) sessions_offset: usize,
    /// 最近一次繪製時逐字稿的區域、捲動偏移與可用寬度 (供滑鼠選取)。
    pub(crate) transcript_area: Rect,
    pub(crate) transcript_offset: usize,
    pub(crate) transcript_inner_w: usize,
    /// 滑鼠拖曳選取範圍：((起始行, 起始欄), (目前行, 目前欄))，
    /// 行是渲染後的絕對行索引，欄是顯示欄位。
    pub(crate) selection: Option<((usize, usize), (usize, usize))>,
    /// 是否擷取滑鼠 (預設開啟，支援應用內拖曳選取；`F2` 關閉以使用終端機原生選取)。
    pub(crate) mouse_capture: bool,
    /// 使用者選擇的版面模式 (僅執行期；T11 的 config.toml 完成後再持久化)。
    pub(crate) layout: LayoutMode,
    /// Bus 欄被隱藏時，是否以浮層暫時顯示。
    pub(crate) bus_overlay: bool,
    /// 三欄版面的右欄顯示 Bus (否則顯示 Project)。
    pub(crate) right_bus: bool,
    /// 最近一次繪製時 Bus 是否可見，以及當時已看過的訊息數 (供未讀計數)。
    pub(crate) bus_visible: bool,
    pub(crate) bus_seen: usize,
    /// 首次收到快照時把既有歷史視為已讀，避免啟動就顯示大量未讀。
    pub(crate) bus_baselined: bool,
    /// 分頁模式下每個分頁的區域與對應 session 索引 (供滑鼠點選)。
    pub(crate) tab_areas: Vec<(Rect, usize)>,
    /// 通知設定與狀態 (T15)。
    pub(crate) notify: crate::notify::Notifier,
    /// 任務視圖：游標 (`tasks::visible_task_ids` 的索引)與清單捲動起點。
    pub(crate) task_cursor: usize,
    pub(crate) task_offset: usize,
    /// 最近一次繪製時任務清單的區域，以及每個可見列對應的任務索引 (供滑鼠)。
    pub(crate) tasks_area: Rect,
    pub(crate) task_rows: Vec<Option<usize>>,
    /// 進行中的搜尋 (只在 `Mode::Search` 期間存在)與上一次的查詢 (供預填)。
    pub(crate) search: Option<crate::search::Search>,
    pub(crate) last_search: String,
    pub(crate) qr_modal: Option<QrModal>,
    pub(crate) ps_modal: Option<crate::ps::PsModal>,
    /// `:quota` 浮層要列出的 agent (開啟時偵測)。
    pub(crate) quota_agents: Vec<String>,
    /// 標題列額度摘要的位置 (點擊開啟 `:quota`)。
    pub(crate) quota_area: Rect,
    pub(crate) should_quit: bool,
}

impl App {
    pub(crate) fn new() -> Self {
        let global_dir = termade_daemon::config_dir();
        let cwd = std::env::current_dir().unwrap_or_default();
        let cfg = termade_daemon::config::Config::load(&global_dir, &cwd);
        let layout = cfg
            .tui
            .layout
            .as_deref()
            .and_then(LayoutMode::parse)
            .unwrap_or(LayoutMode::Two);
        let mut hosts = BTreeMap::new();
        hosts.insert("local".into(), HostData::new("local"));
        for (name, r) in &cfg.remotes {
            if r.enabled {
                hosts.insert(name.clone(), HostData::new(name));
            }
        }
        App {
            state: State::new(),
            hosts,
            active_host: "local".into(),
            host_filter: None,
            selected: 0,
            view: CenterView::Transcript,
            mode: Mode::Normal,
            input: String::new(),
            input_cursor: 0,
            status: tr!("已連線 daemon"),
            spawn_worktree: false,
            spawn_role: Role::Worker,
            spawn_permission: None,
            spawn_bypass_armed: false,
            spawn_target: None,
            spawn_effort: None,
            help_scroll: 0,
            settings: None,
            handoff_from: None,
            at_sel: (String::new(), 0),
            spawn_variants: Vec::new(),
            menu_path: Vec::new(),
            picker: None,
            diff_session: None,
            diff_raw_lines: Vec::new(),
            diff_files: Vec::new(),
            diff_collapsed: BTreeSet::new(),
            diff_discard_armed: None,
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
            show_tools: false,
            disconnected: false,
            daemon_epoch: None,
            home_project: home_project(&cwd),
            scope_here: true,
            input_inner_w: 80,
            prompt_history: crate::history::History::default(),
            command_history: crate::history::History::default(),
            md_cache: None,
            last_rebuild: std::time::Instant::now(),
            rebuild_pending: false,
            input_area: Rect::default(),
            input_sel: None,
            input_nudge: None,
            sessions_area: Rect::default(),
            sessions_offset: 0,
            transcript_area: Rect::default(),
            transcript_offset: 0,
            transcript_inner_w: 0,
            selection: None,
            mouse_capture: true,
            layout,
            bus_overlay: false,
            right_bus: false,
            bus_visible: true,
            bus_seen: 0,
            bus_baselined: false,
            tab_areas: Vec::new(),
            notify: crate::notify::Notifier::from_cfg(&cfg.tui),
            task_cursor: 0,
            task_offset: 0,
            tasks_area: Rect::default(),
            task_rows: Vec::new(),
            search: None,
            last_search: String::new(),
            qr_modal: None,
            ps_modal: None,
            quota_agents: Vec::new(),
            quota_area: Rect::default(),
            should_quit: false,
        }
    }

    pub(crate) fn has_multiple_hosts(&self) -> bool {
        self.hosts.len() > 1
    }

    #[allow(dead_code)]
    pub(crate) fn active_host(&self) -> &str {
        &self.active_host
    }

    pub(crate) fn visible_sessions(&self) -> Vec<(String, SessionId)> {
        if !self.has_multiple_hosts() {
            return self
                .session_ids_single()
                .into_iter()
                .map(|id| (self.active_host.clone(), id))
                .collect();
        }
        let mut out = Vec::new();
        for (host_name, host) in &self.hosts {
            if let Some(f) = &self.host_filter {
                if f != host_name {
                    continue;
                }
            }
            let ids = Self::tree_order_sessions(&host.state, self.scope_here, &self.home_project);
            for id in ids {
                out.push((host_name.clone(), id));
            }
        }
        out
    }

    fn session_ids_single(&self) -> Vec<SessionId> {
        Self::tree_order_sessions(&self.state, self.scope_here, &self.home_project)
    }

    /// 依專案/建立順序做樹狀排序：子 session 緊跟在父 session 後，方便顯示階層。
    fn tree_order_sessions(state: &State, scope_here: bool, home_project: &str) -> Vec<SessionId> {
        use std::collections::{HashMap, HashSet};

        let mut v: Vec<&termade_core::Session> = state
            .sessions_sorted()
            .into_iter()
            .filter(|s| !scope_here || s.project_root() == home_project)
            .collect();
        if !scope_here {
            v.sort_by_key(|s| (s.project_root() != home_project, s.project_root()));
        }

        let present: HashSet<&str> = v.iter().map(|s| s.id.as_str()).collect();
        let mut children: HashMap<String, Vec<SessionId>> = HashMap::new();
        let mut roots: Vec<SessionId> = Vec::new();
        for s in &v {
            if let Some(parent) = s.parent.as_deref() {
                if present.contains(parent) {
                    children
                        .entry(parent.to_string())
                        .or_default()
                        .push(s.id.clone());
                    continue;
                }
            }
            roots.push(s.id.clone());
        }

        let mut out = Vec::with_capacity(v.len());
        fn dfs(
            id: &SessionId,
            children: &HashMap<String, Vec<SessionId>>,
            out: &mut Vec<SessionId>,
        ) {
            out.push(id.clone());
            if let Some(ch) = children.get(id) {
                for c in ch {
                    dfs(c, children, out);
                }
            }
        }
        for r in &roots {
            dfs(r, &children, &mut out);
        }
        out
    }

    /// 取得 session 在可見樹中的深度 (以同 state 內可見的父鏈計算)。
    pub(crate) fn session_tree_depth(&self, host: &str, id: &str) -> usize {
        let state = self
            .hosts
            .get(host)
            .map(|h| &h.state)
            .unwrap_or(&self.state);
        let mut depth = 0usize;
        let mut cur = id;
        while let Some(parent) = state.sessions.get(cur).and_then(|s| s.parent.as_deref()) {
            if state.sessions.contains_key(parent) {
                depth += 1;
                cur = parent;
            } else {
                break;
            }
            if depth > 20 {
                break;
            }
        }
        depth
    }

    pub(crate) fn session_ids(&self) -> Vec<SessionId> {
        self.visible_sessions()
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    pub(crate) fn selected_host(&self) -> String {
        let vis = self.visible_sessions();
        vis.get(self.selected)
            .map(|(h, _)| h.clone())
            .unwrap_or_else(|| self.active_host.clone())
    }

    #[allow(dead_code)]
    pub(crate) fn session_host(&self, session_id: &str) -> Option<String> {
        for (name, host) in &self.hosts {
            if host.state.sessions.contains_key(session_id) {
                return Some(name.clone());
            }
        }
        if self.state.sessions.contains_key(session_id) {
            return Some(self.active_host.clone());
        }
        None
    }

    pub(crate) fn sync_active_host_from_selection(&mut self) {
        let new_host = self.selected_host();
        if new_host != self.active_host {
            self.switch_active_host(&new_host);
        }
    }

    pub(crate) fn switch_active_host(&mut self, new_host: &str) {
        if let Some(entry) = self.hosts.get(new_host) {
            self.active_host = new_host.to_string();
            self.state = entry.state.clone();
            self.daemon_epoch = entry.daemon_epoch.clone();
            self.diff_session = entry.diff_session.clone();
            self.diff_raw_lines = entry.diff_raw_lines.clone();
            self.diff_files = entry.diff_files.clone();
            self.diff_collapsed = entry.diff_collapsed.clone();
            self.diff_discard_armed = None;
            self.diff_lines = entry.diff_lines.clone();
            self.diff_gutter = entry.diff_gutter.clone();
            self.diff_added = entry.diff_added;
            self.diff_removed = entry.diff_removed;
            self.diff_cursor = entry.diff_cursor;
            self.worktrees = entry.worktrees.clone();
            self.transcript_scroll = None;
            self.rebuild_pending = true;
            // 切換主機等於換了一份 bus 歷史；把該主機目前的訊息視為已讀，
            // 之後新進的才算未讀 (不論 Bus 欄當下是否可見)。
            self.bus_seen = self.state.messages.len();
            crate::search::refresh(self);
        }
    }

    pub(crate) fn cycle_host_filter(&mut self) {
        if !self.has_multiple_hosts() {
            return;
        }
        let names: Vec<String> = self.hosts.keys().cloned().collect();
        self.host_filter = match &self.host_filter {
            None => names.first().cloned(),
            Some(curr) => {
                let idx = names.iter().position(|n| n == curr);
                match idx {
                    Some(i) if i + 1 < names.len() => Some(names[i + 1].clone()),
                    _ => None,
                }
            }
        };
        self.status = match &self.host_filter {
            Some(h) => tr!("主機篩選：{h}", h = h),
            None => tr!("主機篩選：全部主機"),
        };
        self.clamp_selection();
        self.sync_active_host_from_selection();
    }

    pub(crate) fn set_host_status(&mut self, host: &str, status: crate::hosts::HostStatus) {
        if let Some(entry) = self.hosts.get_mut(host) {
            entry.status = status;
        }
    }

    pub(crate) fn apply_host_frame(&mut self, host: &str, frame: Frame) {
        let is_active = self.active_host == host;
        if let Some(entry) = self.hosts.get_mut(host) {
            entry.status = crate::hosts::HostStatus::Connected;
        } else {
            let mut h = HostData::new(host);
            h.status = crate::hosts::HostStatus::Connected;
            self.hosts.insert(host.to_string(), h);
        }

        match frame {
            Frame::Ready {
                pid,
                version,
                epoch,
            } => {
                if let Some(entry) = self.hosts.get_mut(host) {
                    entry.daemon_epoch = Some(epoch.clone());
                }
                if is_active {
                    self.status = format!("{host} pid={pid} v{version}");
                    self.daemon_epoch = Some(epoch);
                }
            }
            Frame::Snapshot { state } => {
                self.notify.observe_snapshot(&state);
                if let Some(entry) = self.hosts.get_mut(host) {
                    entry.state = *state.clone();
                }
                if is_active {
                    self.state = *state;
                    if self.bus_baselined {
                        self.bus_seen = self.bus_seen.min(self.state.messages.len());
                    } else {
                        // TUI 剛啟動：快照裡的歷史訊息不算未讀。
                        self.bus_seen = self.state.messages.len();
                        self.bus_baselined = true;
                    }
                    self.clamp_selection();
                }
            }
            Frame::Record { record } => {
                let selected = self.selected_id();
                let now = now_millis();
                if let Some(entry) = self.hosts.get_mut(host) {
                    self.notify.observe(
                        &entry.state,
                        &record,
                        selected.as_deref(),
                        self.scope_here,
                        &self.home_project,
                        now,
                    );
                    entry.state.apply(&record);
                }
                if is_active {
                    let before = self.state.messages.len();
                    self.state.apply(&record);
                    // bus 訊息滿額時會從前端丟棄；已看過的索引要跟著前移，
                    // 否則未讀數會少算 (甚至變成 0)。
                    if matches!(record.event, termade_core::Event::BusMessage { .. }) {
                        let dropped = (before + 1).saturating_sub(self.state.messages.len());
                        self.bus_seen = self.bus_seen.saturating_sub(dropped);
                    }
                    self.clamp_selection();
                }
            }
            Frame::Messages { .. } => {}
            Frame::PermissionDecision { .. } | Frame::Models { .. } => {}
            Frame::Diff { session, diff, .. } => {
                let raw_lines: Vec<String> = diff.lines().map(|l| l.to_string()).collect();
                let files = parse_files(&raw_lines);
                let (added, removed) = diff_stat(&raw_lines);
                let same_session = self.diff_session.as_deref() == Some(session.as_str());
                if let Some(entry) = self.hosts.get_mut(host) {
                    if entry.diff_session.as_deref() != Some(session.as_str()) {
                        entry.diff_collapsed.clear();
                        entry.diff_cursor = 0;
                    } else {
                        entry
                            .diff_collapsed
                            .retain(|p| files.iter().any(|f| &f.path == p));
                    }
                    entry.diff_session = Some(session.clone());
                    entry.diff_raw_lines = raw_lines.clone();
                    entry.diff_files = files.clone();
                    entry.diff_lines = raw_lines.clone();
                    entry.diff_gutter = build_gutter(&raw_lines);
                    entry.diff_added = added;
                    entry.diff_removed = removed;
                }
                if is_active {
                    if !same_session {
                        self.diff_collapsed.clear();
                        self.diff_cursor = 0;
                    } else {
                        self.diff_collapsed
                            .retain(|p| files.iter().any(|f| &f.path == p));
                    }
                    self.diff_discard_armed = None;
                    self.diff_session = Some(session);
                    self.diff_raw_lines = raw_lines;
                    self.diff_files = files;
                    rebuild_visible_diff(self);
                    crate::search::refresh(self);
                }
            }
            Frame::Worktrees { worktrees } => {
                if let Some(entry) = self.hosts.get_mut(host) {
                    entry.worktrees = worktrees.clone();
                }
                if is_active {
                    self.worktrees = worktrees;
                }
            }
            Frame::Ok { note } => {
                if let Some(n) = note {
                    self.status = if self.has_multiple_hosts() {
                        format!("[{host}] {n}")
                    } else {
                        n
                    };
                }
            }
            Frame::Error { message } => {
                self.status = if self.has_multiple_hosts() {
                    tr!("[{host}] 錯誤：{message}", host = host, message = message)
                } else {
                    tr!("錯誤：{message}", message = message)
                };
            }
        }
    }

    /// 目前有幾個不同的專案 (含已結束的 session)。
    pub(crate) fn project_count(&self) -> usize {
        let mut roots: Vec<&str> = self
            .state
            .sessions
            .values()
            .map(|s| s.project_root())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        roots.len()
    }

    /// 選取中 session 的工作目錄 (檔案搜尋與 `@` 補全的根)。
    pub(crate) fn selected_cwd(&self) -> Option<String> {
        let id = self.selected_id()?;
        self.state.sessions.get(&id).map(|s| s.cwd.clone())
    }

    pub(crate) fn search_root(&self) -> std::path::PathBuf {
        crate::attach::search_root(self.selected_cwd().as_deref())
    }

    /// 選取 session 的 agent 種類 id (設定檔 `[agents.<id>]` 的鍵)。
    pub(crate) fn selected_kind_id(&self) -> Option<String> {
        let id = self.selected_id()?;
        self.state
            .sessions
            .get(&id)
            .map(|s| s.kind.id().to_string())
    }

    pub(crate) fn selected_id(&self) -> Option<SessionId> {
        self.session_ids().get(self.selected).cloned()
    }

    /// 回傳目前選取 session 尚未回覆的核准請求 id。
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
            return 0;
        }
        if !self.scope_here {
            return self.state.messages.len().saturating_sub(self.bus_seen);
        }
        // `bus_seen` 是未過濾清單的長度；只看本專案時，後面的訊息仍要再過濾一次。
        self.state
            .messages
            .iter()
            .enumerate()
            .filter(|(i, m)| *i >= self.bus_seen && self.message_in_scope(m))
            .count()
    }

    /// Bus 訊息是否屬於目前檢視的專案。`scope_here` 關閉時全部顯示。
    ///
    /// 判斷順序：
    /// * 發送者屬於本專案 → 顯示。
    /// * 直接訊息：收件者屬於本專案 → 顯示；對不到 session (已移除)時無法判斷，保留。
    /// * 廣播／頻道沒有收件者清單，只能看發送者；發送者已不在清單時保留。
    fn message_in_scope(&self, m: &BusMessage) -> bool {
        if !self.scope_here {
            return true;
        }
        let here = |sid: &str| {
            self.state
                .sessions
                .get(sid)
                .is_some_and(|s| s.project_root() == self.home_project)
        };
        if here(&m.from) {
            return true;
        }
        match &m.to {
            Target::Direct(t) => {
                let sid = self
                    .state
                    .resolve_session_id(t)
                    .unwrap_or_else(|| t.clone());
                match self.state.sessions.get(&sid) {
                    Some(s) => s.project_root() == self.home_project,
                    // 對不到 session (已移除)：無法判斷，保留。
                    None => true,
                }
            }
            Target::Broadcast | Target::Channel(_) => !self.state.sessions.contains_key(&m.from),
        }
    }

    /// Bus 面板要顯示的訊息 (依 `scope_here` 過濾)。
    pub(crate) fn visible_messages(&self) -> Vec<&BusMessage> {
        self.state
            .messages
            .iter()
            .filter(|m| self.message_in_scope(m))
            .collect()
    }

    /// 切換 Bus 顯示：三欄版面把右欄在 Project／Bus 間切換，其他版面切換浮層。
    pub(crate) fn toggle_bus(&mut self) {
        let on = if self.layout == LayoutMode::Three {
            self.right_bus = !self.right_bus;
            self.right_bus
        } else {
            !self.bus_overlay
        };
        self.bus_overlay = on;
        self.status = if on {
            tr!("Bus：顯示 (b 或 Esc 關閉)")
        } else {
            tr!("Bus：隱藏")
        };
    }

    pub(crate) fn cycle_layout(&mut self) {
        self.set_layout(self.layout.next());
    }

    pub(crate) fn set_layout(&mut self, mode: LayoutMode) {
        self.layout = mode;
        self.selection = None;
        self.status = tr!("版面：{layout}", layout = mode.id());
    }

    pub(crate) fn clamp_selection(&mut self) {
        let n = self.session_ids().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
        self.sync_active_host_from_selection();
    }

    /// `Normal` 模式收到可列印字元：不打字，但明確提示要先切進輸入模式。
    /// 使用者常忘了要先按 `i` 或 Enter，看到畫面沒反應會以為程式卡住。
    pub(crate) fn nudge_input(&mut self) {
        self.input_nudge = Some(std::time::Instant::now());
        self.status = tr!(
            "目前是命令模式：按 {keys} 才能輸入文字",
            keys = self.input_key_hint()
        );
    }

    /// 是否正在顯示「請先按 i 或 Enter」的提示 (按下未綁定的字元後短暫出現)。
    pub(crate) fn input_nudging(&self) -> bool {
        self.input_nudge
            .is_some_and(|t| t.elapsed() < INPUT_NUDGE_DURATION)
    }

    /// 這個視圖按 Enter 是否會進入輸入模式 (`Diff`／`Tasks`／`Grid` 的 Enter 另有用途)。
    pub(crate) fn enter_opens_input(&self) -> bool {
        !matches!(
            self.view,
            CenterView::Diff | CenterView::Tasks | CenterView::Grid
        )
    }

    /// 提示文字裡的按鍵說明：Enter 不能進輸入模式的視圖只寫 `i`。
    pub(crate) fn input_key_hint(&self) -> String {
        if self.enter_opens_input() {
            tr!("i 或 Enter")
        } else {
            tr!("i")
        }
    }

    /// 進入 `Prompt` 輸入模式 (清空輸入框並把游標放到開頭)。
    pub(crate) fn start_prompt(&mut self) {
        self.mode = Mode::Prompt;
        self.input.clear();
        self.input_cursor = 0;
        self.input_sel = None;
    }

    /// 取得目前 diff 游標所在行的位置描述 (`檔案 :: 行內容`)。
    pub(crate) fn current_diff_location(&self) -> String {
        if self.diff_lines.is_empty() {
            return "diff".into();
        }
        let idx = self.diff_cursor.min(self.diff_lines.len() - 1);
        let current_file = if let Some((_, _, path)) = crate::diff::current_file_progress(self) {
            path
        } else {
            let mut file = "unknown";
            for l in &self.diff_lines[..=idx] {
                if let Some(rest) = l.strip_prefix("+++ b/") {
                    file = rest.trim();
                } else if let Some(rest) = l.strip_prefix("diff --git ") {
                    file = rest.trim();
                }
            }
            file.to_string()
        };
        let line_text = self.diff_lines[idx].trim();
        format!("{current_file} (L{}: {})", idx + 1, truncate(line_text, 48))
    }
}

pub(crate) fn main_pane_text(app: &App) -> String {
    match app.view {
        CenterView::Transcript => {
            let active = app
                .selected_id()
                .and_then(|id| app.state.sessions.get(&id))
                .map(|s| s.active_tools.as_slice())
                .unwrap_or(&[]);
            app.selected_id()
                .and_then(|id| app.state.transcripts.get(&id))
                .map(|buf| {
                    visible_transcript(buf.iter(), app.show_thoughts, app.show_tools, active)
                        .join("\n")
                })
                .unwrap_or_default()
        }
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
        CenterView::Tasks => crate::tasks::tasks_text(app),
        CenterView::Grid => String::new(),
    }
}

struct ToolRun<'a> {
    calls: usize,
    ok: usize,
    last: Cow<'a, str>,
    prev_completed: Option<Cow<'a, str>>,
}

/// 依顯示設定過濾逐字稿：隱藏思考行；未展開工具時，把連續的工具呼叫／成功結果
/// 摺成一行摘要 (失敗 `✗` 行保留，錯誤不該被藏起來)。
/// 若最後一筆工具呼叫尚未完成 (`active_tools` 非空)，不摺入統計行，而是獨立顯示於最下方。
pub(crate) fn visible_transcript<'a>(
    lines: impl Iterator<Item = &'a String>,
    show_thoughts: bool,
    show_tools: bool,
    active_tools: &[termade_core::ActiveTool],
) -> Vec<Cow<'a, str>> {
    let mut out: Vec<Cow<'a, str>> = Vec::with_capacity(lines.size_hint().0);
    let mut run: Option<ToolRun<'a>> = None;
    fn flush<'a>(out: &mut Vec<Cow<'a, str>>, run: &mut Option<ToolRun<'a>>) {
        if let Some(r) = run.take() {
            if r.calls == 1 {
                out.push(r.last);
            } else {
                let last_str = r.last.trim_start_matches("▸ ");
                out.push(Cow::Owned(tr!(
                    "▸ 工具 ×{calls} (✓{ok})· 最後：{last}",
                    calls = r.calls,
                    ok = r.ok,
                    last = last_str
                )));
            }
        }
    }
    for l in lines {
        if !show_thoughts && l.starts_with("💭 ") {
            continue;
        }
        if show_tools {
            out.push(Cow::Borrowed(l.as_str()));
        } else if l.starts_with("▸ ") {
            let (calls, ok, prev) = match run.take() {
                Some(r) => (r.calls + 1, r.ok, Some(r.last)),
                None => (1, 0, None),
            };
            run = Some(ToolRun {
                calls,
                ok,
                last: Cow::Borrowed(l.as_str()),
                prev_completed: prev,
            });
        } else if l.starts_with("  ✓") {
            if let Some(r) = run.as_mut() {
                r.ok += 1;
            }
        } else {
            flush(&mut out, &mut run);
            out.push(Cow::Borrowed(l.as_str()));
        }
    }

    if !active_tools.is_empty() {
        let tool = active_tools.last().unwrap();
        let elapsed_secs = (termade_core::now_millis().saturating_sub(tool.started_ms)) / 1000;
        let tag = if tool.is_subagent {
            "subagent"
        } else {
            &tr!("執行中")
        };
        let sum_part = if tool.summary.is_empty() {
            String::new()
        } else {
            format!(": {}", tool.summary)
        };
        let active_line = format!("▸ [{tag}] {}{sum_part} ({elapsed_secs}s)", tool.name);

        if show_tools {
            if let Some(last) = out.last_mut() {
                if last.starts_with("▸ ") {
                    *last = Cow::Owned(active_line);
                } else {
                    out.push(Cow::Owned(active_line));
                }
            } else {
                out.push(Cow::Owned(active_line));
            }
        } else if let Some(r) = run.take() {
            let completed_calls = r.calls.saturating_sub(1);
            if completed_calls == 1 {
                if let Some(p) = r.prev_completed {
                    out.push(p);
                }
            } else if completed_calls > 1 {
                let last_str = r
                    .prev_completed
                    .as_deref()
                    .unwrap_or("")
                    .trim_start_matches("▸ ");
                out.push(Cow::Owned(tr!(
                    "▸ 工具 ×{calls} (✓{ok})· 最後：{last}",
                    calls = completed_calls,
                    ok = r.ok,
                    last = last_str
                )));
            }
            out.push(Cow::Owned(active_line));
        } else {
            out.push(Cow::Owned(active_line));
        }
    } else {
        flush(&mut out, &mut run);
    }
    out
}

/// 切換是否展開每一筆工具呼叫。
pub(crate) fn toggle_tools(app: &mut App) {
    app.show_tools = !app.show_tools;
    app.md_cache = None;
    app.selection = None;
    app.status = if app.show_tools {
        tr!("工具：展開 (每筆呼叫都顯示，按 O 摺疊)")
    } else {
        tr!("工具：摺疊 (連續呼叫合成一行，按 O 展開)")
    };
}

/// 切換是否顯示 agent 的思考行 (`💭`)。
pub(crate) fn toggle_thoughts(app: &mut App) {
    app.show_thoughts = !app.show_thoughts;
    app.md_cache = None;
    app.selection = None;
    app.status = if app.show_thoughts {
        tr!("思考顯示：開 (顯示 💭 行)")
    } else {
        tr!("思考顯示：關 (已隱藏 💭 行，按 T 顯示)")
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
        app.status = tr!("逐字稿：跟隨最新");
    } else {
        app.transcript_scroll = Some(next);
        app.status = tr!("逐字稿：第 {next} 行 (End 回到最新)", next = next);
    }
}

/// 以「行」為單位捲動 (滑鼠滾輪用)，到底時恢復跟隨最新。
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

/// 依顯示寬度截斷 (中文等寬字元佔 2 欄)，超出時以 `…` 結尾。
pub(crate) fn truncate_width(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if UnicodeWidthStr::width(s.as_str()) <= max {
        return s;
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    out
}

pub(crate) fn short(id: &str) -> String {
    id.rsplit('_')
        .next()
        .unwrap_or(id)
        .chars()
        .take(6)
        .collect()
}

/// 專案根目錄：cwd 所在 git repo 的頂層，不在 repo 內就用 cwd 本身。
pub(crate) fn home_project(cwd: &std::path::Path) -> String {
    let top = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    top.unwrap_or_else(|| cwd.to_string_lossy().to_string())
}

/// 專案顯示名稱：路徑最後一段；兩個專案同名時帶上上一層以區分。
pub(crate) fn project_name(root: &str, all_roots: &[&str]) -> String {
    let base = |p: &str| {
        std::path::Path::new(p)
            .file_name()
            .map_or_else(|| p.to_string(), |n| n.to_string_lossy().to_string())
    };
    let name = base(root);
    let clash = all_roots.iter().any(|r| *r != root && base(r) == name);
    if clash {
        let parent = std::path::Path::new(root)
            .parent()
            .map(|p| base(&p.to_string_lossy()))
            .unwrap_or_default();
        format!("{parent}/{name}")
    } else {
        name
    }
}

/// 專案標籤顏色：同一專案永遠同色。
pub(crate) fn project_color(root: &str) -> ratatui::style::Color {
    use ratatui::style::Color;
    const PALETTE: [Color; 6] = [
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightCyan,
        Color::LightRed,
    ];
    let h = root
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(b as u32));
    PALETTE[h as usize % PALETTE.len()]
}

/// 切換「只看本專案／全部專案」。
pub(crate) fn toggle_scope(app: &mut App, here: Option<bool>) {
    app.scope_here = here.unwrap_or(!app.scope_here);
    app.selected = 0;
    app.md_cache = None;
    app.selection = None;
    app.status = if app.scope_here {
        tr!(
            "只顯示本專案的 session、Bus 訊息與任務 ({project})，P 顯示全部",
            project = project_name(&app.home_project, &[])
        )
    } else {
        tr!("顯示全部專案的 session、Bus 訊息與任務，P 只看本專案")
    };
}
