//! Helix 風格的 Space 選單 (which-key)與 spawn 選擇器 (agent → model → prompt)。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ertangle_core::{AgentKind, Attachment, Role, SessionId, SessionStatus};
use std::path::{Path, PathBuf};
use tr::tr;

use crate::app::{App, Mode};
use crate::commands::execute_command;
use crate::hosts::HostRouter;

/// 選單項目的動作。
#[derive(Clone, Copy)]
pub(crate) enum Act {
    /// 進入子選單。
    Menu,
    /// 執行命令 (等同在 `:` 輸入)。
    Cmd(&'static str),
    /// 開啟 spawn 選擇器。
    Spawn,
    /// 開啟交接選擇器 (選接手的 agent 與 model)。
    Handoff,
    /// 切到輸入模式並預填文字。
    Prefill(Mode, &'static str),
}

#[derive(Clone, Copy)]
pub(crate) struct Item {
    pub key: char,
    pub label: &'static str,
    pub act: Act,
}

const fn it(key: char, label: &'static str, act: Act) -> Item {
    Item { key, label, act }
}

/// 依目前路徑回傳選單標題與項目；路徑不存在回傳 `None`。
pub(crate) fn menu_items(path: &[char]) -> Option<(&'static str, Vec<Item>)> {
    Some(match path {
        [] => (
            "SPC",
            vec![
                it('s', "session ▸", Act::Menu),
                it('g', "git / worktree ▸", Act::Menu),
                it('v', "view ▸", Act::Menu),
                it('m', "message ▸", Act::Menu),
                it(
                    ',',
                    ertangle_core::tr_noop!("設定 (預設 model／effort／權限…)"),
                    Act::Cmd("settings"),
                ),
                it(
                    'w',
                    ertangle_core::tr_noop!("Web 版 (網址與 QR Code)"),
                    Act::Cmd("web qr"),
                ),
                it(
                    'y',
                    ertangle_core::tr_noop!("複製內容 (Transcript 預設最新訊息)"),
                    Act::Cmd("copy"),
                ),
                it(
                    '?',
                    ertangle_core::tr_noop!("說明 (所有按鍵與命令)"),
                    Act::Cmd("help"),
                ),
                it(
                    'q',
                    ertangle_core::tr_noop!("離開 (保留 daemon)"),
                    Act::Cmd("quit"),
                ),
                it(
                    'x',
                    ertangle_core::tr_noop!("結束本專案所有 session"),
                    Act::Cmd("stop"),
                ),
                it(
                    'Q',
                    ertangle_core::tr_noop!("關閉共用 daemon 並離開"),
                    Act::Cmd("shutdown"),
                ),
                it(
                    'R',
                    ertangle_core::tr_noop!("重啟共用 daemon (自動重連)"),
                    Act::Cmd("restart"),
                ),
                it(
                    'P',
                    ertangle_core::tr_noop!("切換專案 (已知專案清單)"),
                    Act::Cmd("project open"),
                ),
                it(
                    'p',
                    ertangle_core::tr_noop!("瀏覽資料夾，開啟新專案"),
                    Act::Cmd("project browse"),
                ),
            ],
        ),
        ['s'] => (
            "SPC s · session",
            vec![
                it(
                    'n',
                    ertangle_core::tr_noop!("新增 agent… (選 agent 與 model)"),
                    Act::Spawn,
                ),
                it(
                    'b',
                    ertangle_core::tr_noop!("啟動多 Agent 團隊範本 (playbook)"),
                    Act::Cmd("playbook"),
                ),
                it(
                    'h',
                    ertangle_core::tr_noop!("交接給其他 agent (額度用完時)"),
                    Act::Handoff,
                ),
                it(
                    'r',
                    ertangle_core::tr_noop!("接續已結束的 session (接回原對話)"),
                    Act::Cmd("resume"),
                ),
                it(
                    'm',
                    ertangle_core::tr_noop!("切換 model (執行中)"),
                    Act::Prefill(Mode::Command, "model "),
                ),
                it(
                    'e',
                    ertangle_core::tr_noop!("切換 effort (執行中)"),
                    Act::Prefill(Mode::Command, "effort "),
                ),
                it(
                    'c',
                    ertangle_core::tr_noop!("中斷目前回合"),
                    Act::Cmd("cancel"),
                ),
                it(
                    'p',
                    ertangle_core::tr_noop!("處理程序與 Subagent"),
                    Act::Cmd("ps"),
                ),
                it(
                    'q',
                    ertangle_core::tr_noop!("各 agent 額度 (用量與重置時間)"),
                    Act::Cmd("quota"),
                ),
                it(
                    'k',
                    ertangle_core::tr_noop!("結束 session"),
                    Act::Cmd("kill"),
                ),
                it('z', ertangle_core::tr_noop!("休眠"), Act::Cmd("sleep")),
                it('w', ertangle_core::tr_noop!("喚醒"), Act::Cmd("wake")),
                it(
                    'x',
                    ertangle_core::tr_noop!("移除已結束的 session"),
                    Act::Cmd("rm"),
                ),
                it(
                    'X',
                    ertangle_core::tr_noop!("清除所有已結束的 session"),
                    Act::Cmd("clean"),
                ),
            ],
        ),
        ['g'] => (
            "SPC g · git / worktree",
            vec![
                it('d', "diff", Act::Cmd("diff")),
                it('w', "worktrees", Act::Cmd("worktrees")),
                it(
                    'm',
                    ertangle_core::tr_noop!("merge 回主工作區"),
                    Act::Cmd("merge"),
                ),
                it(
                    'n',
                    ertangle_core::tr_noop!("merge 乾跑 (dry-run)"),
                    Act::Cmd("merge --dry-run"),
                ),
                it(
                    'r',
                    ertangle_core::tr_noop!("合併並委派解衝突"),
                    Act::Cmd("merge --resolve"),
                ),
                it(
                    'c',
                    ertangle_core::tr_noop!("gc 已合併的 worktree"),
                    Act::Cmd("gc"),
                ),
                it(
                    'p',
                    ertangle_core::tr_noop!("封存已結束且已合併的 session"),
                    Act::Cmd("prune"),
                ),
            ],
        ),
        ['v'] => (
            "SPC v · view",
            vec![
                it('o', "overview", Act::Cmd("overview")),
                it('k', ertangle_core::tr_noop!("任務視圖"), Act::Cmd("tasks")),
                it('l', ertangle_core::tr_noop!("切換版面"), Act::Cmd("layout")),
                it(
                    'g',
                    ertangle_core::tr_noop!("網格檢視 (一次看 4 個 session)"),
                    Act::Cmd("grid"),
                ),
                it(
                    'r',
                    ertangle_core::tr_noop!("原始文字／Markdown"),
                    Act::Cmd("raw"),
                ),
                it(
                    't',
                    ertangle_core::tr_noop!("顯示／隱藏思考"),
                    Act::Cmd("thoughts"),
                ),
                it(
                    'x',
                    ertangle_core::tr_noop!("展開／摺疊工具呼叫"),
                    Act::Cmd("tools"),
                ),
                it(
                    'p',
                    ertangle_core::tr_noop!("只看本專案／全部專案"),
                    Act::Cmd("scope"),
                ),
                it(
                    'b',
                    ertangle_core::tr_noop!("顯示／隱藏 Bus"),
                    Act::Cmd("bus"),
                ),
            ],
        ),
        ['m'] => (
            "SPC m · message",
            vec![
                it(
                    'm',
                    ertangle_core::tr_noop!("訊息給選取的 session"),
                    Act::Prefill(Mode::Message, ""),
                ),
                it(
                    'b',
                    ertangle_core::tr_noop!("廣播給所有人"),
                    Act::Prefill(Mode::Command, "broadcast "),
                ),
                it(
                    't',
                    ertangle_core::tr_noop!("新增任務"),
                    Act::Prefill(Mode::Command, "task "),
                ),
                it(
                    'f',
                    ertangle_core::tr_noop!("附加檔案／圖片… (@路徑，Tab 自動完成)"),
                    Act::Prefill(Mode::Prompt, "@"),
                ),
                it(
                    'e',
                    ertangle_core::tr_noop!("Shell 命令 (在 session 工作目錄執行)"),
                    Act::Prefill(Mode::Prompt, "!"),
                ),
            ],
        ),
        _ => return None,
    })
}

fn close(app: &mut App) {
    app.mode = Mode::Normal;
    app.menu_path.clear();
}

pub(crate) fn open_menu(app: &mut App) {
    app.mode = Mode::Menu;
    app.menu_path.clear();
}

pub(crate) fn handle_menu_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => close(app),
        KeyCode::Backspace => {
            if app.menu_path.pop().is_none() {
                close(app);
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let Some((_, items)) = menu_items(&app.menu_path) else {
                close(app);
                return Ok(());
            };
            let Some(item) = items.iter().find(|i| i.key == c).copied() else {
                app.status = tr!("選單沒有 `{c}`(Backspace 返回、Esc 關閉)", c = c);
                return Ok(());
            };
            match item.act {
                Act::Menu => app.menu_path.push(c),
                Act::Cmd(cmd) => {
                    close(app);
                    execute_command(app, writer, cmd)?;
                }
                Act::Spawn => {
                    close(app);
                    start_spawn_picker(app);
                }
                Act::Handoff => {
                    close(app);
                    start_handoff_picker(app);
                }
                Act::Prefill(mode, text) => {
                    close(app);
                    app.mode = mode;
                    app.input = text.to_string();
                    app.input_cursor = text.chars().count();
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// spawn 選擇器
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ResumeScope {
    #[default]
    Here,
    All,
}

#[derive(Clone)]
pub(crate) enum PickStage {
    Agent,
    Model(AgentKind),
    HandoffRole {
        kind: AgentKind,
        model: Option<String>,
    },
    /// Answer an interactive question.
    Question {
        id: String,
        session: ertangle_core::SessionId,
        multi_select: bool,
        selected: std::collections::HashSet<String>,
        /// 同一組 (group) 必須一起回答的所有問題，依 id 排序。
        questions: Vec<ertangle_core::UserQuestion>,
        /// 目前顯示的問題在 `questions` 的位置。
        index: usize,
        /// 已回答問題的 (session, id, answers)；最後一題答完才一起送出。
        collected: Vec<(ertangle_core::SessionId, String, Vec<String>)>,
        /// 是否正在輸入自訂答案 (文字不會過濾選項)。
        custom_mode: bool,
        custom_text: String,
        /// Cursor inside `custom_text`, as a char index.
        custom_cursor: usize,
    },
    /// 選擇多 Agent 團隊範本。
    Playbook,
    /// 選擇要接續的已結束 session。
    Resume,
    /// Pick the project to switch to.
    Project,
    /// Browse the local filesystem to pick a project folder.
    Directory {
        current: PathBuf,
    },
    /// Confirm resuming and sending prompt to an ended or sleeping session (T88).
    #[expect(dead_code, reason = "對話框狀態變體之部分欄位保留供後續流程讀取")]
    ConfirmResumePrompt {
        session: SessionId,
        label: String,
        status: SessionStatus,
        resumable: bool,
        text: String,
        attachments: Vec<Attachment>,
    },
    /// Offer to create a new session or resume a previous one when no session is selected (T96).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "對話框狀態變體之部分欄位保留供後續流程讀取")
    )]
    NoSessionPrompt {
        text: String,
        attachments: Vec<Attachment>,
        can_resume: bool,
    },
}

/// 專案選擇器裡「瀏覽本機資料夾」項目的值。用控制字元避免與真實路徑撞名。
/// 資料夾選擇器裡「使用目前資料夾」項目的值。
pub(crate) const SELECT_VALUE: &str = "\u{0}select";
/// 資料夾選擇器裡「上層資料夾」項目的值。
pub(crate) const PARENT_VALUE: &str = "..";
/// 資料夾瀏覽器的「家目錄」捷徑列。
pub(crate) const HOME_VALUE: &str = "\u{0}home";

/// 列出 `current` 底下的資料夾，加上上層與「使用目前資料夾」兩列。
pub(crate) fn directory_items(current: &Path) -> Vec<(String, String)> {
    // 「使用此資料夾」放最上面，進到想要的資料夾後不用捲到底；其後依序是家目錄、上層。
    let mut items = vec![(tr!("* 使用此資料夾"), SELECT_VALUE.to_string())];
    // 已經在家目錄時不用再列 `~/`。
    if home_dir().is_some_and(|home| !same_dir(&home, current)) {
        items.push((tr!("  ~/"), HOME_VALUE.to_string()));
    }
    items.push((tr!("  ../"), PARENT_VALUE.to_string()));
    if let Ok(rd) = std::fs::read_dir(current) {
        let mut dirs: Vec<String> = rd
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        dirs.sort();
        for dir in dirs {
            items.push((format!("  {dir}/"), dir));
        }
    }
    items
}

/// 兩個路徑是否指向同一個資料夾 (連結與 `..` 先正規化)。
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_dir())
}

/// `:project browse` / `Space p`：從目前專案所在位置瀏覽本機資料夾，選一個開成新專案。
pub(crate) fn start_folder_browser(app: &mut App) {
    start_directory_picker(app, PathBuf::from(&app.active_project));
}

fn is_directory_shortcut(value: &str) -> bool {
    matches!(value, SELECT_VALUE | PARENT_VALUE | HOME_VALUE)
}

/// 進入 (或重新整理) 資料夾選擇器。
fn start_directory_picker(app: &mut App, current: PathBuf) {
    let items = directory_items(&current);
    let title = tr!("資料夾 {path}", path = current.display());
    app.picker = Some(Picker {
        stage: PickStage::Directory { current },
        resume_scope: ResumeScope::default(),
        title,
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

pub(crate) struct Picker {
    pub stage: PickStage,
    pub resume_scope: ResumeScope,
    pub title: String,
    /// (顯示文字, 值)；值為空字串代表「預設」。
    pub items: Vec<(String, String)>,
    pub filter: String,
    /// 過濾輸入框的游標位置 (字元索引)。
    pub filter_cursor: usize,
    pub cursor: usize,
    /// 背景查詢 agent 可用 model 的結果 (查到後併入 `items`)。
    pub pending: Option<std::sync::mpsc::Receiver<ertangle_adapter::models::ModelQuery>>,
    pub loading: bool,
    /// 背景查詢 model 時遇到的問題 (例如 agy 尚未登入)，顯示在清單下方。
    pub load_error: Option<String>,
    /// Antigravity 的 model id 把 effort 寫在尾巴 (`-low`／`-medium`…)：清單只列基底名稱，
    /// 這裡記錄每個基底有哪些 effort 變體。
    pub group_effort: bool,
    pub variants: std::collections::HashMap<String, Vec<String>>,
}

/// 把 `base-medium` 這類 id 拆成 (base, effort)；沒有 effort 尾巴就回 `None`。
pub(crate) fn split_effort_suffix(id: &str) -> Option<(&str, &str)> {
    let (base, suffix) = id.rsplit_once('-')?;
    ertangle_adapter::EFFORT_LEVELS
        .contains(&suffix)
        .then_some((base, suffix))
}

impl Picker {
    /// 輸入框為空時顯示的提示。
    pub(crate) fn placeholder(&self) -> String {
        match self.stage {
            PickStage::Directory { .. } => tr!("輸入過濾資料夾，~ 回家目錄"),
            PickStage::Project => tr!("輸入過濾專案，或直接輸入路徑"),
            PickStage::ConfirmResumePrompt { .. } | PickStage::NoSessionPrompt { .. } => {
                tr!("選擇以確認或取消")
            }
            _ => tr!("輸入以過濾"),
        }
    }

    /// 顯示在選擇器右下角的按鍵提示 (目前只有資料夾瀏覽器用)。
    pub(crate) fn hint(&self) -> Option<String> {
        match &self.stage {
            PickStage::Directory { .. } => Some(tr!("Enter 進入／選取 · Esc 取消")),
            PickStage::ConfirmResumePrompt { .. } | PickStage::NoSessionPrompt { .. } => {
                Some(tr!("Enter 選擇 · Esc 取消"))
            }
            _ => None,
        }
    }

    /// 把背景查詢的結果併進清單 (每次繪製前呼叫)。
    pub(crate) fn poll_pending(&mut self) {
        let Some(rx) = &self.pending else { return };
        match rx.try_recv() {
            Ok(query) => {
                self.load_error = query.problem;
                let found = query.models;
                // 先依基底名稱分組，再決定要不要合併：只有出現兩個以上 effort 變體才視為
                // 「同一個 model 的不同 effort」(ACP 的 model 選項不含 effort 尾巴)。
                let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
                for (label, value) in found {
                    let key = match split_effort_suffix(&value) {
                        Some((base, _)) if self.group_effort => base.to_string(),
                        _ => value.clone(),
                    };
                    match groups.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, v)) => v.push((label, value)),
                        None => groups.push((key, vec![(label, value)])),
                    }
                }
                for (base, members) in groups {
                    let merge = self.group_effort
                        && members.len() >= 2
                        && members
                            .iter()
                            .all(|(_, v)| split_effort_suffix(v).is_some());
                    if merge {
                        for (_, v) in &members {
                            if let Some((_, eff)) = split_effort_suffix(v) {
                                self.variants
                                    .entry(base.clone())
                                    .or_default()
                                    .push(eff.to_string());
                            }
                        }
                        // 顯示名稱去掉尾端的 `(Medium)` 這類 effort 標記。
                        let name = members
                            .first()
                            .and_then(|(s, _)| s.split_once("  ("))
                            .map(|(_, n)| n.trim_end_matches(')'))
                            .and_then(|n| n.rsplit_once(" ("))
                            .map(|(n, _)| n.to_string());
                        let label = match name {
                            Some(n) => format!("{base}  ({n})"),
                            None => base.clone(),
                        };
                        if let Some(existing) = self.items.iter_mut().find(|(_, v)| *v == base) {
                            if existing.0 == existing.1 {
                                existing.0 = label;
                            }
                        } else {
                            self.items.push((label, base));
                        }
                    } else {
                        for (label, value) in members {
                            if let Some(existing) = self.items.iter_mut().find(|(_, v)| *v == value)
                            {
                                if existing.0 == existing.1 {
                                    existing.0 = label;
                                }
                            } else {
                                self.items.push((label, value));
                            }
                        }
                    }
                }
                if let PickStage::Model(k) = &self.stage {
                    let cached: Vec<(String, String)> = self
                        .items
                        .iter()
                        .filter(|(_, v)| !v.is_empty())
                        .cloned()
                        .collect();
                    update_live_models_cache(k.clone(), cached);
                }
                self.loading = false;
                self.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(_) => {
                self.loading = false;
                self.pending = None;
            }
        }
    }

    pub(crate) fn visible(&self) -> Vec<&(String, String)> {
        let f = self.filter.to_ascii_lowercase();
        // 資料夾瀏覽器：過濾只比對子資料夾名稱 (捷徑列的說明文字含路徑，會誤中)，
        // 有過濾字時捷徑列先隱藏。
        if matches!(self.stage, PickStage::Directory { .. }) && !f.is_empty() {
            return self
                .items
                .iter()
                .filter(|(_, v)| !is_directory_shortcut(v) && v.to_ascii_lowercase().contains(&f))
                .collect();
        }
        self.items
            .iter()
            .filter(|(label, value)| {
                f.is_empty()
                    || label.to_ascii_lowercase().contains(&f)
                    || value.to_ascii_lowercase().contains(&f)
            })
            .collect()
    }
}

type LiveModelsCache = std::sync::RwLock<std::collections::HashMap<String, Vec<(String, String)>>>;

static LIVE_CACHE: std::sync::LazyLock<LiveModelsCache> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashMap::new()));

pub(crate) fn update_live_models_cache(kind: AgentKind, models: Vec<(String, String)>) {
    if let Ok(mut cache) = LIVE_CACHE.write() {
        cache.insert(kind.id().to_string(), models);
    }
}

/// 某個 agent 可選的 model：預設 + 內建清單 + 快取清單 + config.toml 的 `models`／`model`。
pub(crate) fn model_choices(kind: &AgentKind) -> Vec<(String, String)> {
    let mut out = vec![(tr!("預設 (依 config.toml)"), String::new())];
    match kind {
        AgentKind::ClaudeCode => {
            out.extend(
                ertangle_adapter::models::CLAUDE_MODELS
                    .iter()
                    .map(|(id, name)| {
                        let name = ertangle_core::tr_dyn!(name);
                        (format!("{id}  ({name})"), id.to_string())
                    }),
            );
        }
        AgentKind::Antigravity => {
            out.extend(
                ertangle_adapter::models::ANTIGRAVITY_MODELS
                    .iter()
                    .map(|(id, name)| {
                        let name = ertangle_core::tr_dyn!(name);
                        (format!("{id}  ({name})"), id.to_string())
                    }),
            );
        }
        _ => {}
    }
    let key = kind.id().to_string();
    let need_fetch = if matches!(kind, AgentKind::ClaudeCode) {
        false
    } else {
        LIVE_CACHE
            .read()
            .map(|c| !c.contains_key(&key))
            .unwrap_or(false)
    };
    if let Ok(cache) = LIVE_CACHE.read() {
        if let Some(cached) = cache.get(&key) {
            for (label, val) in cached {
                if !out.iter().any(|(_, v)| v == val) {
                    out.push((label.clone(), val.clone()));
                }
            }
        }
    }
    if need_fetch {
        if let Ok(mut cache) = LIVE_CACHE.write() {
            cache.entry(key).or_default();
        }
        let k = kind.clone();
        std::thread::spawn(move || {
            let found = ertangle_adapter::models::query_models(&k);
            if !found.is_empty() {
                update_live_models_cache(k, found);
            }
        });
    }
    let mut values: Vec<String> = Vec::new();
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    if let Some(a) = cfg.agents.get(kind.id()) {
        for m in a.model.iter().chain(a.models.iter()) {
            if !values.contains(m) && !out.iter().any(|(_, v)| v == m) {
                values.push(m.clone());
            }
        }
    }
    out.extend(values.into_iter().map(|v| (v.clone(), v)));
    out
}

pub(crate) use ertangle_adapter::models::query_models;

/// 交接：選接手的 agent 與 model，選完直接送出 (簡報由 daemon 產生)。
pub(crate) fn start_handoff_picker(app: &mut App) {
    let Some(src) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return;
    };
    start_spawn_picker(app);
    app.handoff_from = Some(src);
    if let Some(p) = app.picker.as_mut() {
        p.title = tr!("交接給哪個 agent");
    }
}

/// 開啟問題選擇器；若問題屬於某個 group，會一併帶入同 session 同 group 的其他問題，
/// 依 id 排序後逐題回答。
pub(crate) fn start_question_picker(app: &mut App, q: &ertangle_core::UserQuestion) {
    let mut questions: Vec<ertangle_core::UserQuestion> = match &q.group {
        Some(g) => app
            .state
            .pending_questions
            .values()
            .filter(|o| o.session == q.session && o.group.as_ref() == Some(g))
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    if !questions.iter().any(|o| o.id == q.id) {
        questions.push(q.clone());
    }
    questions.sort_by(|a, b| a.id.cmp(&b.id));
    let index = questions.iter().position(|o| o.id == q.id).unwrap_or(0);
    let mut picker = Picker {
        stage: PickStage::Question {
            id: String::new(),
            session: q.session.clone(),
            multi_select: false,
            selected: Default::default(),
            questions,
            index,
            collected: Vec::new(),
            custom_mode: false,
            custom_text: String::new(),
            custom_cursor: 0,
        },
        resume_scope: ResumeScope::default(),
        title: String::new(),
        items: Vec::new(),
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    };
    load_question(&mut picker, index);
    app.mode = Mode::Pick;
    app.picker = Some(picker);
}

/// 把目前選擇器內的所有問題標為「已略過」，避免自動重新開啟 (手動 Enter 仍可開)。
fn dismiss_open_questions(app: &mut App) {
    if let Some(Picker {
        stage: PickStage::Question { questions, .. },
        ..
    }) = app.picker.as_ref()
    {
        app.dismissed_questions
            .extend(questions.iter().map(|q| q.id.clone()));
    }
}

/// 把 `questions[index]` 載入選擇器 (重設游標、勾選與自訂輸入)。
fn load_question(p: &mut Picker, index: usize) {
    let PickStage::Question {
        id,
        session,
        multi_select,
        selected,
        questions,
        index: cur,
        custom_mode,
        custom_text,
        custom_cursor,
        ..
    } = &mut p.stage
    else {
        return;
    };
    let Some(q) = questions.get(index) else {
        return;
    };
    *cur = index;
    *id = q.id.clone();
    *session = q.session.clone();
    *multi_select = q.multi_select;
    selected.clear();
    custom_text.clear();
    *custom_cursor = 0;
    *custom_mode = q.options.is_empty() && q.custom;
    p.items = q
        .options
        .iter()
        .map(|o| (o.label.clone(), o.label.clone()))
        .collect();
    p.title = if questions.len() > 1 {
        tr!("問題 ({n}/{total})", n = index + 1, total = questions.len())
    } else {
        tr!("問題")
    };
    p.filter.clear();
    p.filter_cursor = 0;
    p.cursor = 0;
}

/// 目前問題的游標列數 (選項 + 「自訂答案」列)。
fn question_rows(p: &Picker) -> usize {
    let PickStage::Question {
        questions, index, ..
    } = &p.stage
    else {
        return 0;
    };
    let custom = questions.get(*index).is_some_and(|q| q.custom);
    p.items.len() + usize::from(custom)
}

/// 記下目前問題的答案；還有下一題就前進，否則把整組答案送出。
fn finish_question(app: &mut App, writer: &mut HostRouter, answers: Vec<String>) -> Result<()> {
    let Some(p) = app.picker.as_mut() else {
        return Ok(());
    };
    let next = {
        let PickStage::Question {
            id,
            session,
            collected,
            questions,
            index,
            ..
        } = &mut p.stage
        else {
            return Ok(());
        };
        collected.push((session.clone(), id.clone(), answers));
        (*index + 1 < questions.len()).then_some(*index + 1)
    };
    if let Some(next) = next {
        load_question(p, next);
        return Ok(());
    }
    let collected = match std::mem::replace(&mut p.stage, PickStage::Agent) {
        PickStage::Question { collected, .. } => collected,
        _ => Vec::new(),
    };
    // daemon 清掉問題前不要再自動開啟。
    app.dismissed_questions
        .extend(collected.iter().map(|(_, id, _)| id.clone()));
    app.picker = None;
    app.mode = Mode::Normal;
    for (session, id, answers) in collected {
        crate::send(
            writer,
            &ertangle_protocol::Request::AnswerQuestion {
                session,
                id,
                answers,
            },
        )?;
    }
    app.status = tr!("已送出問題回覆");
    Ok(())
}

/// 問題選擇器的按鍵處理：不做 type-to-filter，自訂答案走獨立的輸入模式。
fn handle_question_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    let Some(p) = app.picker.as_mut() else {
        return Ok(());
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let rows = question_rows(p);
    let n_opts = p.items.len();
    let (multi, custom_mode) = match &p.stage {
        PickStage::Question {
            multi_select,
            custom_mode,
            ..
        } => (*multi_select, *custom_mode),
        _ => return Ok(()),
    };

    if custom_mode {
        return handle_custom_input(app, writer, key);
    }
    let toggle = |p: &mut Picker, i: usize| {
        let Some((_, val)) = p.items.get(i).cloned() else {
            return;
        };
        if let PickStage::Question { selected, .. } = &mut p.stage {
            if !selected.remove(&val) {
                selected.insert(val);
            }
        }
    };
    let enter_custom = |p: &mut Picker| {
        if let PickStage::Question {
            custom_mode,
            custom_text,
            custom_cursor,
            ..
        } = &mut p.stage
        {
            *custom_mode = true;
            *custom_cursor = custom_text.chars().count();
        }
    };
    match key.code {
        KeyCode::Esc => {
            // 不回答：問題仍留在 pending。
            dismiss_open_questions(app);
            app.picker = None;
            app.mode = Mode::Normal;
        }
        KeyCode::Down | KeyCode::Tab => {
            if rows > 0 {
                p.cursor = (p.cursor + 1) % rows;
            }
        }
        KeyCode::Up | KeyCode::BackTab => {
            if rows > 0 {
                p.cursor = (p.cursor + rows - 1) % rows;
            }
        }
        KeyCode::Char('n') if ctrl => {
            if rows > 0 {
                p.cursor = (p.cursor + 1) % rows;
            }
        }
        KeyCode::Char('p') if ctrl => {
            if rows > 0 {
                p.cursor = (p.cursor + rows - 1) % rows;
            }
        }
        KeyCode::Char(' ') if multi && !ctrl => {
            if p.cursor < n_opts {
                let c = p.cursor;
                toggle(p, c);
            } else if p.cursor < rows {
                enter_custom(p);
            }
        }
        KeyCode::Char('s') if !ctrl => return skip_question(app, writer),
        KeyCode::Char(c @ '1'..='9') if !ctrl => {
            let i = (c as usize) - ('1' as usize);
            if i >= rows {
                return Ok(());
            }
            p.cursor = i;
            if i >= n_opts {
                enter_custom(p);
            } else if multi {
                toggle(p, i);
            } else if let Some((_, v)) = p.items.get(i).cloned() {
                return finish_question(app, writer, vec![v]);
            }
        }
        KeyCode::Enter => {
            if p.cursor >= n_opts && p.cursor < rows {
                enter_custom(p);
                return Ok(());
            }
            let highlighted = p.items.get(p.cursor).map(|(_, v)| v.clone());
            let answers = if multi {
                let mut ans = ordered_selection(p);
                if ans.is_empty() {
                    ans.extend(highlighted);
                }
                ans
            } else {
                highlighted.into_iter().collect()
            };
            return finish_question(app, writer, answers);
        }
        _ => {}
    }
    Ok(())
}

/// 跳過群組中的目前問題 (送出空答案)；群組至少要有一題有答案，所以不能全部跳過。
fn skip_question(app: &mut App, writer: &mut HostRouter) -> Result<()> {
    let Some(p) = app.picker.as_ref() else {
        return Ok(());
    };
    let PickStage::Question {
        questions,
        index,
        collected,
        ..
    } = &p.stage
    else {
        return Ok(());
    };
    if questions.len() < 2 {
        return Ok(());
    }
    let last = *index + 1 == questions.len();
    if last && collected.iter().all(|(_, _, a)| a.is_empty()) {
        app.status = tr!("至少要回答群組中的一題");
        return Ok(());
    }
    finish_question(app, writer, Vec::new())
}

/// 已勾選的選項，依問題給定的選項順序排列。
fn ordered_selection(p: &Picker) -> Vec<String> {
    let PickStage::Question { selected, .. } = &p.stage else {
        return Vec::new();
    };
    p.items
        .iter()
        .filter(|(_, v)| selected.contains(v))
        .map(|(_, v)| v.clone())
        .collect()
}

/// Previous word boundary (char index) before `cur`: skip spaces, then the word.
fn word_start(chars: &[char], cur: usize) -> usize {
    let mut i = cur.min(chars.len());
    while i > 0 && chars.get(i - 1).is_some_and(|c| c.is_whitespace()) {
        i -= 1;
    }
    while i > 0 && chars.get(i - 1).is_some_and(|c| !c.is_whitespace()) {
        i -= 1;
    }
    i
}

/// Next word boundary (char index) after `cur`: skip the word, then spaces.
fn word_end(chars: &[char], cur: usize) -> usize {
    let mut i = cur.min(chars.len());
    while i < chars.len() && chars.get(i).is_some_and(|c| !c.is_whitespace()) {
        i += 1;
    }
    while i < chars.len() && chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Remove chars in `[from, to)` (char indices).
fn remove_chars(text: &mut String, from: usize, to: usize) {
    let b0 = crate::input::byte_index(text, from);
    let b1 = crate::input::byte_index(text, to);
    text.replace_range(b0..b1, "");
}

/// Insert `s` at char cursor `cur` and advance the cursor.
pub(crate) fn insert_at(text: &mut String, cur: &mut usize, s: &str) {
    let b = crate::input::byte_index(text, *cur);
    text.insert_str(b, s);
    *cur += s.chars().count();
}

/// Single-line editor shared by the custom-answer row. Returns true when the
/// key was an editing/movement key (and was consumed).
pub(crate) fn edit_line(text: &mut String, cur: &mut usize, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    *cur = (*cur).min(n);
    match key.code {
        KeyCode::Left if ctrl || alt => *cur = word_start(&chars, *cur),
        KeyCode::Right if ctrl || alt => *cur = word_end(&chars, *cur),
        KeyCode::Left => *cur = cur.saturating_sub(1),
        KeyCode::Right => *cur = (*cur + 1).min(n),
        KeyCode::Home => *cur = 0,
        KeyCode::End => *cur = n,
        KeyCode::Char('a') if ctrl => *cur = 0,
        KeyCode::Char('e') if ctrl => *cur = n,
        KeyCode::Backspace if alt => {
            let s = word_start(&chars, *cur);
            remove_chars(text, s, *cur);
            *cur = s;
        }
        KeyCode::Backspace => {
            if *cur > 0 {
                remove_chars(text, *cur - 1, *cur);
                *cur -= 1;
            }
        }
        KeyCode::Delete => {
            if *cur < n {
                remove_chars(text, *cur, *cur + 1);
            }
        }
        KeyCode::Char('w') if ctrl => {
            let s = word_start(&chars, *cur);
            remove_chars(text, s, *cur);
            *cur = s;
        }
        KeyCode::Char('u') if ctrl => {
            remove_chars(text, 0, *cur);
            *cur = 0;
        }
        KeyCode::Char('k') if ctrl => remove_chars(text, *cur, n),
        KeyCode::Char(c) if !ctrl && !alt => {
            let mut buf = [0u8; 4];
            insert_at(text, cur, c.encode_utf8(&mut buf));
        }
        _ => return false,
    }
    true
}

/// 過濾輸入框的編輯；文字有變動時游標列回到第一項。回傳是否吃掉這個按鍵。
fn edit_line_filter(p: &mut Picker, key: &KeyEvent) -> bool {
    let before = p.filter.clone();
    if !edit_line(&mut p.filter, &mut p.filter_cursor, key) {
        return false;
    }
    if p.filter != before {
        p.cursor = 0;
    }
    true
}

/// Paste into the custom-answer row when it is active. Returns false if not.
pub(crate) fn paste_custom(app: &mut App, text: &str) -> bool {
    let Some(Picker {
        stage:
            PickStage::Question {
                custom_mode: true,
                custom_text,
                custom_cursor,
                ..
            },
        ..
    }) = app.picker.as_mut()
    else {
        // 一般選擇器：貼進過濾輸入框 (問題選擇器的選項模式不接受貼上)。
        let Some(p) = app.picker.as_mut() else {
            return false;
        };
        if matches!(p.stage, PickStage::Question { .. }) {
            return false;
        }
        let flat = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        insert_at(&mut p.filter, &mut p.filter_cursor, &flat);
        p.cursor = 0;
        return true;
    };
    let flat = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
    insert_at(custom_text, custom_cursor, &flat);
    true
}

/// Reconcile the open question picker with `app.state.pending_questions`:
/// drop questions resolved elsewhere (web, another client), advance to the
/// next remaining one, and close the picker when none remain. Returns true if
/// the picker changed.
pub(crate) fn reconcile_question_picker(app: &mut App) -> bool {
    let Some(p) = app.picker.as_mut() else {
        return false;
    };
    let PickStage::Question {
        id,
        questions,
        index,
        collected,
        ..
    } = &mut p.stage
    else {
        return false;
    };
    let pending = &app.state.pending_questions;
    if questions.iter().all(|q| pending.contains_key(&q.id)) {
        return false;
    }
    let old_index = *index;
    let current_alive = pending.contains_key(id.as_str());
    let kept_before = questions
        .iter()
        .take(old_index)
        .filter(|q| pending.contains_key(&q.id))
        .count();
    questions.retain(|q| pending.contains_key(&q.id));
    collected.retain(|(_, cid, _)| pending.contains_key(cid));
    if questions.is_empty() {
        app.picker = None;
        app.mode = Mode::Normal;
        app.status = tr!("問題已在其他地方回答");
        return true;
    }
    if current_alive {
        // Same question stays open; only its position/total changed.
        let cur = id.clone();
        let pos = questions.iter().position(|q| q.id == cur).unwrap_or(0);
        *index = pos;
        let total = questions.len();
        p.title = if total > 1 {
            tr!("問題 ({n}/{total})", n = pos + 1, total = total)
        } else {
            tr!("問題")
        };
    } else {
        let next = kept_before.min(questions.len() - 1);
        load_question(p, next);
        app.status = tr!("問題已在其他地方回答");
    }
    true
}

/// 自訂答案輸入模式：所有字元 (含空白) 都是答案的一部分。
fn handle_custom_input(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    let Some(p) = app.picker.as_mut() else {
        return Ok(());
    };
    let has_options = !p.items.is_empty();
    let in_group = matches!(&p.stage, PickStage::Question { questions, .. } if questions.len() > 1);
    let multi = matches!(
        p.stage,
        PickStage::Question {
            multi_select: true,
            ..
        }
    );
    let PickStage::Question {
        custom_mode,
        custom_text,
        custom_cursor,
        ..
    } = &mut p.stage
    else {
        return Ok(());
    };
    if edit_line(custom_text, custom_cursor, &key) {
        return Ok(());
    }
    match key.code {
        KeyCode::Esc => {
            if has_options {
                *custom_mode = false;
                custom_text.clear();
                *custom_cursor = 0;
            } else {
                // 沒有選項可退回：直接離開，問題仍 pending。
                dismiss_open_questions(app);
                app.picker = None;
                app.mode = Mode::Normal;
            }
        }
        KeyCode::Enter => {
            let text = custom_text.trim().to_string();
            if text.is_empty() {
                if has_options {
                    *custom_mode = false;
                    return Ok(());
                }
                // 只有自由輸入的選填欄位：群組內留空 Enter 即跳過。
                if in_group {
                    return skip_question(app, writer);
                }
                return Ok(());
            }
            let mut answers = if multi {
                ordered_selection(p)
            } else {
                Vec::new()
            };
            answers.push(text);
            return finish_question(app, writer, answers);
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn start_spawn_picker(app: &mut App) {
    app.handoff_from = None;
    // 使用者設定的 worker 順序排前面，其餘維持內建順序。
    let order = ertangle_daemon::config::Config::load_file(&crate::settings::config_path())
        .defaults
        .worker_order;
    let rank = |id: &str| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    let mut kinds = AgentKind::builtins().to_vec();
    kinds.sort_by_key(|k| rank(k.id()));
    let items = kinds
        .iter()
        .map(|k| (k.id().to_string(), k.id().to_string()))
        .collect();
    app.picker = Some(Picker {
        stage: PickStage::Agent,
        resume_scope: ResumeScope::default(),
        title: tr!("選擇 agent"),
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

/// `Space s b`／`:playbook`：列出內建與自訂團隊範本，選中後再輸入目標需求。
pub(crate) fn start_playbook_picker(app: &mut App) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    let items: Vec<(String, String)> = cfg
        .playbooks()
        .into_iter()
        .map(|(name, pb)| (format!("{name}  ({})", pb.description), name))
        .collect();
    if items.is_empty() {
        app.status = tr!("沒有可用的 playbook");
        return;
    }
    app.picker = Some(Picker {
        stage: PickStage::Playbook,
        resume_scope: ResumeScope::default(),
        title: tr!("選擇團隊範本 (選完輸入目標需求)"),
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

pub(crate) fn format_compact_age(now_ms: u64, created_ms: u64) -> String {
    let secs = now_ms.saturating_sub(created_ms) / 1000;
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}m", secs / 60)
    }
}

/// Rebuild resume picker items: filter by scope, add parent annotation and project prefix.
pub(crate) fn rebuild_resume_items(app: &App, scope: ResumeScope) -> Vec<(String, String)> {
    let mut ended: Vec<(&ertangle_core::Session, bool)> = Vec::new();
    // 收集本機與多主機的已結束及封存歷史 session
    for (host_name, h) in &app.hosts {
        if app.host_filter.as_ref().is_some_and(|f| f != host_name) {
            continue;
        }
        for s in h.state.sessions.values().filter(|s| !s.status.is_live()) {
            if !ended.iter().any(|(existing, _)| existing.id == s.id) {
                ended.push((s, false));
            }
        }
        for s in h
            .state
            .archived_sessions
            .values()
            .filter(|s| s.native_id.is_some())
        {
            if !ended.iter().any(|(existing, _)| existing.id == s.id) {
                ended.push((s, true));
            }
        }
    }
    for s in app.state.sessions.values().filter(|s| !s.status.is_live()) {
        if !ended.iter().any(|(existing, _)| existing.id == s.id) {
            ended.push((s, false));
        }
    }
    for s in app
        .state
        .archived_sessions
        .values()
        .filter(|s| s.native_id.is_some())
    {
        if !ended.iter().any(|(existing, _)| existing.id == s.id) {
            ended.push((s, true));
        }
    }

    // Build an id -> label map covering live, ended, and archived sessions on all hosts.
    let mut parent_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for h in app.hosts.values() {
        for s in h
            .state
            .sessions
            .values()
            .chain(h.state.archived_sessions.values())
        {
            parent_map.insert(s.id.clone(), s.label.clone());
        }
    }
    for s in app
        .state
        .sessions
        .values()
        .chain(app.state.archived_sessions.values())
    {
        parent_map.insert(s.id.clone(), s.label.clone());
    }

    // Collect all project roots so project_name can disambiguate identical names.
    let mut all_roots: Vec<String> = Vec::new();
    for (s, _) in &ended {
        let root = s.project_root().to_string();
        if !all_roots.contains(&root) {
            all_roots.push(root);
        }
    }
    let all_roots_refs: Vec<&str> = all_roots.iter().map(|s| s.as_str()).collect();

    if scope == ResumeScope::Here {
        ended.retain(|(s, _)| s.project_root() == app.home_project);
    }

    ended.sort_by_key(|(a, _)| std::cmp::Reverse(a.created_ms));
    let now_ms = ertangle_core::now_millis();
    ended
        .into_iter()
        .map(|(s, is_archived)| {
            let kind_model = match &s.model {
                Some(m) if !m.is_empty() => format!("{}:{}", s.kind.id(), m),
                _ => s.kind.id().to_string(),
            };
            let age = format_compact_age(now_ms, s.created_ms);
            let resumable = if s.native_id.is_some() { "↻" } else { "–" };
            let parent_suffix = match &s.parent {
                Some(pid) => {
                    let plabel = parent_map
                        .get(pid)
                        .cloned()
                        .unwrap_or_else(|| crate::app::short(pid));
                    format!("  ↳ {plabel}")
                }
                None => String::new(),
            };
            let proj_prefix = if scope == ResumeScope::All {
                let pname = crate::app::project_name(s.project_root(), &all_roots_refs);
                format!("[{pname}] ")
            } else {
                String::new()
            };
            let tag_part = if is_archived {
                let tag = tr!("封存");
                format!(" [{tag}]")
            } else {
                String::new()
            };
            let label = format!(
                "{proj_prefix}{} {}{tag_part}  {}  {}  {}{parent_suffix}",
                s.status.glyph(),
                s.label,
                kind_model,
                age,
                resumable
            );
            (label, s.id.clone())
        })
        .collect()
}

pub(crate) fn start_resume_picker(app: &mut App) {
    let all_items = rebuild_resume_items(app, ResumeScope::All);
    if all_items.is_empty() {
        app.status = tr!("沒有可接續的 session (已結束且具備對話 id)");
        return;
    }

    let resume_scope = if app.scope_here {
        ResumeScope::Here
    } else {
        ResumeScope::All
    };
    let items = if resume_scope == ResumeScope::All {
        all_items
    } else {
        rebuild_resume_items(app, ResumeScope::Here)
    };

    let cursor = app
        .selected_id()
        .and_then(|sel| items.iter().position(|(_, id)| id == &sel))
        .unwrap_or(0);

    app.picker = Some(Picker {
        stage: PickStage::Resume,
        resume_scope,
        title: tr!("接續 session (Tab 切換專案範圍 · Enter 確認 · Esc 取消)"),
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

/// 開啟接續已停止／休眠 session 並送出 prompt 的確認選擇器 (T88)。
pub(crate) fn start_confirm_resume_prompt(
    app: &mut App,
    session: SessionId,
    label: String,
    status: SessionStatus,
    resumable: bool,
    text: String,
    attachments: Vec<Attachment>,
) {
    let items = if status == SessionStatus::Sleeping || resumable {
        vec![
            (tr!("是，接續並送出"), "confirm".to_string()),
            (tr!("否，保留草稿並取消"), "cancel".to_string()),
        ]
    } else {
        vec![(
            tr!("保留草稿並取消 (此 session 無可接續的對話 id)"),
            "cancel".to_string(),
        )]
    };

    let status_str = match status {
        SessionStatus::Sleeping => tr!("休眠中"),
        SessionStatus::Exited => tr!("已結束"),
        SessionStatus::Failed => tr!("失敗"),
        _ => format!("{status:?}"),
    };
    let title = tr!(
        "Session \"{label}\" 已停止 ({status})，是否接續並送出 prompt？",
        label = label,
        status = status_str
    );

    app.picker = Some(Picker {
        stage: PickStage::ConfirmResumePrompt {
            session,
            label,
            status,
            resumable,
            text,
            attachments,
        },
        resume_scope: ResumeScope::Here,
        title,
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

/// Restore a cancelled prompt draft back into the input box.
fn restore_prompt_draft(app: &mut App, text: String) {
    app.input = text;
    app.input_cursor = app.input.chars().count();
    app.mode = Mode::Prompt;
    app.status = tr!("已保留草稿並取消");
}

/// Resume an ended session and deliver the pending prompt (and any attachments).
fn send_resume_with_prompt(
    app: &mut App,
    writer: &mut HostRouter,
    session: SessionId,
    text: String,
    attachments: Vec<Attachment>,
) -> Result<()> {
    app.select_or_pend(&session);
    if attachments.is_empty() {
        crate::send(
            writer,
            &ertangle_protocol::Request::Resume {
                session,
                prompt: Some(text),
            },
        )?;
        app.status = tr!("已接續並送出 prompt");
    } else {
        let n = attachments.len();
        crate::send(
            writer,
            &ertangle_protocol::Request::Resume {
                session: session.clone(),
                prompt: None,
            },
        )?;
        crate::send(
            writer,
            &ertangle_protocol::Request::Prompt {
                session,
                text,
                attachments,
                queue: false,
            },
        )?;
        app.status = tr!("已接續並送出 prompt (含 {n} 個附件)", n = n);
    }
    Ok(())
}

/// Offer to create a new session or resume an ended one when submitting a prompt with no session (T96).
pub(crate) fn start_no_session_prompt(app: &mut App, text: String, attachments: Vec<Attachment>) {
    let can_resume = !rebuild_resume_items(app, ResumeScope::All).is_empty();
    let mut items = vec![(tr!("建立新 session 並送出"), "create".to_string())];
    if can_resume {
        items.push((tr!("接續先前的 session 並送出"), "resume".to_string()));
    }
    items.push((tr!("保留草稿並取消"), "cancel".to_string()));

    app.picker = Some(Picker {
        stage: PickStage::NoSessionPrompt {
            text,
            attachments,
            can_resume,
        },
        resume_scope: ResumeScope::Here,
        title: tr!("目前沒有作用中的 session，要如何處理這則 prompt？"),
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

/// `:project` / `:project open`: list configured and observed project roots, then switch to the chosen one.
pub(crate) fn start_project_picker(app: &mut App) {
    let global_dir = ertangle_daemon::config_dir();
    let cfg = ertangle_daemon::config::Config::load(&global_dir, &app.active_project);

    let mut roots: Vec<String> = Vec::new();
    let mut add_root = |r: &str| {
        let trimmed = r.trim();
        if !trimmed.is_empty() && !roots.iter().any(|existing| existing == trimmed) {
            roots.push(trimmed.to_string());
        }
    };

    add_root(&app.home_project);
    for r in &cfg.tui.projects {
        add_root(r);
    }
    for s in app.state.sessions.values() {
        add_root(s.project_root());
    }
    for s in app.state.archived_sessions.values() {
        add_root(s.project_root());
    }
    for h in app.hosts.values() {
        for s in h.state.sessions.values() {
            add_root(s.project_root());
        }
        for s in h.state.archived_sessions.values() {
            add_root(s.project_root());
        }
    }

    if roots.is_empty() {
        app.status = tr!("沒有可用的專案清單");
        return;
    }

    let all_roots_refs: Vec<&str> = roots.iter().map(|s| s.as_str()).collect();
    let items: Vec<(String, String)> = roots
        .iter()
        .map(|r| {
            let pname = crate::app::project_name(r, &all_roots_refs);
            let is_curr = r == &app.home_project;
            let mark = if is_curr { "* " } else { "  " };
            let label = format!("{mark}{:<16} {r}", pname);
            (label, r.clone())
        })
        .collect();
    let cursor = items
        .iter()
        .position(|(_, path)| path == &app.home_project)
        .unwrap_or(0);

    app.picker = Some(Picker {
        stage: PickStage::Project,
        resume_scope: ResumeScope::default(),
        title: tr!("選擇專案 (Enter 確認 · Esc 取消)"),
        items,
        filter: String::new(),
        filter_cursor: 0,
        cursor,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
        load_error: None,
    });
    app.mode = Mode::Pick;
}

pub(crate) fn finish_picker(
    app: &mut App,
    writer: &mut HostRouter,
    kind: AgentKind,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    app.picker = None;
    app.mode = Mode::Normal;
    if let Some(src) = app.handoff_from.take() {
        crate::send(
            writer,
            &ertangle_protocol::Request::Handoff {
                session: src,
                kind: kind.clone(),
                model: model.clone(),
                effort,
                note: None,
                role: None,
                parent: None,
                force: false,
            },
        )?;
        app.status = tr!(
            "已要求交接給 {target} (舊 session 會停止，新 agent 沿用同一個工作目錄)",
            target = format!(
                "{}{}",
                kind.id(),
                model.map(|m| format!(":{m}")).unwrap_or_default()
            )
        );
        app.spawn_variants.clear();
        return Ok(());
    }
    app.status = tr!(
        "spawn → {target}：輸入 prompt 後 Enter (可留空)，Ctrl-R 切換角色，Ctrl-F 切換 effort",
        target = format!(
            "{}{}{}",
            kind.id(),
            model
                .as_deref()
                .map(|m| format!(":{m}"))
                .unwrap_or_default(),
            effort
                .as_deref()
                .map(|e| format!(" effort={e}"))
                .unwrap_or_default(),
        )
    );
    app.spawn_target = Some((kind, model));
    app.spawn_effort = effort;
    app.mode = Mode::Spawn;
    if let Some((text, attachments)) = app.pending_prompt_draft.take() {
        app.input = text;
        app.input_cursor = app.input.chars().count();
        app.pending_spawn_attachments = attachments;
    } else {
        app.input.clear();
        app.input_cursor = 0;
        app.pending_spawn_attachments.clear();
    }
    app.spawn_worktree = false;
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    app.spawn_role = cfg
        .defaults
        .role
        .as_deref()
        .map(Role::parse)
        .unwrap_or(Role::Coordinator);
    app.spawn_permission = None;
    app.spawn_bypass_armed = false;
    Ok(())
}

pub(crate) fn handle_pick_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    if matches!(
        app.picker.as_ref().map(|p| &p.stage),
        Some(PickStage::ConfirmResumePrompt { .. } | PickStage::NoSessionPrompt { .. })
    ) && key.code == KeyCode::Esc
    {
        if let Some(p) = app.picker.take() {
            match p.stage {
                PickStage::ConfirmResumePrompt { text, .. }
                | PickStage::NoSessionPrompt { text, .. } => restore_prompt_draft(app, text),
                _ => {}
            }
        }
        return Ok(());
    }
    let Some(p) = app.picker.as_mut() else {
        app.mode = Mode::Normal;
        return Ok(());
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('q')) {
        app.should_quit = true;
        return Ok(());
    }
    if matches!(p.stage, PickStage::Question { .. }) {
        return handle_question_key(app, writer, key);
    }
    // 資料夾選擇器：有過濾字時 Esc 先清掉過濾；過濾清空後 Esc 關閉、Backspace 往上層。
    if matches!(p.stage, PickStage::Directory { .. })
        && !p.filter.is_empty()
        && key.code == KeyCode::Esc
    {
        p.filter.clear();
        p.filter_cursor = 0;
        p.cursor = 0;
        return Ok(());
    }
    if matches!(p.stage, PickStage::Directory { .. })
        && (key.code == KeyCode::Esc || (key.code == KeyCode::Backspace && p.filter.is_empty()))
    {
        let (current, parent) = match &p.stage {
            PickStage::Directory { current } => (
                current.clone(),
                current
                    .parent()
                    .filter(|parent| *parent != current.as_path())
                    .map(Path::to_path_buf),
            ),
            _ => (PathBuf::from("/"), None),
        };
        let code = key.code;
        app.picker = None;
        if code == KeyCode::Esc {
            app.mode = Mode::Normal;
        } else {
            start_directory_picker(app, parent.unwrap_or(current));
        }
        return Ok(());
    }
    let n = p.visible().len();
    match key.code {
        KeyCode::Esc => {
            app.picker = None;
            if let Some((text, _)) = app.pending_prompt_draft.take() {
                restore_prompt_draft(app, text);
            } else {
                app.mode = Mode::Normal;
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            if matches!(p.stage, PickStage::Resume) {
                let new_scope = match p.resume_scope {
                    ResumeScope::Here => ResumeScope::All,
                    ResumeScope::All => ResumeScope::Here,
                };
                let old_cursor = p.cursor;
                let items = rebuild_resume_items(app, new_scope);
                if let Some(p) = app.picker.as_mut() {
                    p.resume_scope = new_scope;
                    p.items = items;
                    let n = p.visible().len();
                    p.cursor = if n > 0 { old_cursor.min(n - 1) } else { 0 };
                }
                return Ok(());
            }
            if n > 0 {
                if key.code == KeyCode::Tab {
                    p.cursor = (p.cursor + 1) % n;
                } else {
                    p.cursor = (p.cursor + n - 1) % n;
                }
            }
        }
        KeyCode::Down => {
            if n > 0 {
                p.cursor = (p.cursor + 1) % n;
            }
        }
        KeyCode::Up => {
            if n > 0 {
                p.cursor = (p.cursor + n - 1) % n;
            }
        }
        KeyCode::Char('t') if ctrl => {
            if matches!(p.stage, PickStage::Resume) {
                let new_scope = match p.resume_scope {
                    ResumeScope::Here => ResumeScope::All,
                    ResumeScope::All => ResumeScope::Here,
                };
                let old_cursor = p.cursor;
                let items = rebuild_resume_items(app, new_scope);
                if let Some(p) = app.picker.as_mut() {
                    p.resume_scope = new_scope;
                    p.items = items;
                    let n = p.visible().len();
                    p.cursor = if n > 0 { old_cursor.min(n - 1) } else { 0 };
                }
                return Ok(());
            }
        }
        KeyCode::Char('n') if ctrl => {
            if n > 0 {
                p.cursor = (p.cursor + 1) % n;
            }
        }
        KeyCode::Char('p') if ctrl => {
            if n > 0 {
                p.cursor = (p.cursor + n - 1) % n;
            }
        }
        // 過濾輸入框是一般文字輸入框：游標移動、刪字、Ctrl-A/E/U/W/K 都可用。
        _ if edit_line_filter(p, &key) => {}
        KeyCode::Enter => {
            let chosen = p.visible().get(p.cursor).map(|(_, v)| v.clone());
            let custom = p.filter.trim().to_string();
            match p.stage.clone() {
                PickStage::Agent => {
                    // 沒有符合的項目時，也接受別名 (例如 agy、oc)。
                    let chosen =
                        chosen.or_else(|| AgentKind::parse(&custom).map(|k| k.id().to_string()));
                    let Some(kind) = chosen.and_then(|v| AgentKind::parse(&v)) else {
                        app.status = tr!("沒有符合的 agent");
                        return Ok(());
                    };
                    p.title = tr!(
                        "{agent} 的 model (輸入可過濾；無符合時 Enter 使用輸入值)",
                        agent = kind.id()
                    );
                    p.items = model_choices(&kind);
                    p.filter.clear();
                    p.filter_cursor = 0;
                    p.cursor = 0;
                    p.load_error = None;
                    // 向 CLI 查詢可用 model (不阻塞介面)。
                    let (tx, rx) = std::sync::mpsc::channel();
                    let k = kind.clone();
                    if matches!(
                        k,
                        AgentKind::OpenCode | AgentKind::Antigravity | AgentKind::Codex
                    ) {
                        std::thread::spawn(move || {
                            // 主介面若已關閉或放棄選單則忽略
                            let _ =
                                tx.send(ertangle_adapter::models::query_raw_models_detailed(&k));
                        });
                        p.pending = Some(rx);
                        p.loading = true;
                        p.group_effort = matches!(kind, AgentKind::Antigravity);
                    }
                    p.stage = PickStage::Model(kind);
                }
                PickStage::Model(kind) => {
                    // 有符合的項目就用它；清單是空的但有輸入，就當成自訂 model。
                    let model = match chosen {
                        Some(v) if !v.is_empty() => Some(v),
                        Some(_) => None,
                        None if !custom.is_empty() => Some(custom),
                        None => None,
                    };
                    if let Some(src_id) = app.handoff_from.clone() {
                        let cur_role = app
                            .state
                            .sessions
                            .get(&src_id)
                            .map(|s| s.role.clone())
                            .unwrap_or(Role::Worker);
                        let is_coord = cur_role == Role::Coordinator;
                        let running_children_count = app
                            .state
                            .sessions
                            .values()
                            .filter(|s| s.parent.as_deref() == Some(&src_id) && s.status.is_live())
                            .count();
                        let role_choices = [Role::Coordinator, Role::Worker, Role::Reviewer];
                        let mut items = Vec::new();
                        let mut default_idx = 0;
                        for (i, r) in role_choices.iter().enumerate() {
                            if *r == cur_role {
                                default_idx = i;
                            }
                            let label = if is_coord
                                && *r != Role::Coordinator
                                && running_children_count > 0
                            {
                                tr!(
                                    "{role} (警告：仍有 {count} 個執行中子 session)",
                                    role = r.id(),
                                    count = running_children_count
                                )
                            } else {
                                r.id().to_string()
                            };
                            items.push((label, r.id().to_string()));
                        }
                        p.title = tr!("交接後的角色 (Enter 確認 · Esc 取消)");
                        p.items = items;
                        p.filter.clear();
                        p.filter_cursor = 0;
                        p.cursor = default_idx;
                        p.pending = None;
                        p.loading = false;
                        p.load_error = None;
                        p.group_effort = false;
                        p.stage = PickStage::HandoffRole { kind, model };
                        return Ok(());
                    }
                    app.spawn_variants = model
                        .as_deref()
                        .and_then(|m| p.variants.get(m).cloned())
                        .unwrap_or_default();
                    finish_picker(app, writer, kind, model, None)?;
                }
                PickStage::HandoffRole { kind, model } => {
                    let chosen_role_str = chosen.unwrap_or_else(|| {
                        if custom.is_empty() {
                            "worker".into()
                        } else {
                            custom
                        }
                    });
                    let new_role = Role::parse(&chosen_role_str);
                    app.picker = None;
                    app.mode = Mode::Normal;
                    if let Some(src) = app.handoff_from.take() {
                        let is_coord = app
                            .state
                            .sessions
                            .get(&src)
                            .is_some_and(|s| s.role == Role::Coordinator);
                        let has_running_children = app
                            .state
                            .sessions
                            .values()
                            .any(|s| s.parent.as_deref() == Some(&src) && s.status.is_live());
                        let force =
                            is_coord && new_role != Role::Coordinator && has_running_children;
                        crate::send(
                            writer,
                            &ertangle_protocol::Request::Handoff {
                                session: src,
                                kind: kind.clone(),
                                model: model.clone(),
                                effort: None,
                                note: None,
                                role: Some(new_role.clone()),
                                parent: None,
                                force,
                            },
                        )?;
                        app.status = tr!(
                            "已要求交接給 {target} (角色：{role})",
                            target = format!(
                                "{}{}",
                                kind.id(),
                                model.map(|m| format!(":{m}")).unwrap_or_default()
                            ),
                            role = new_role.id()
                        );
                        app.spawn_variants.clear();
                    }
                }
                PickStage::Playbook => {
                    let Some(name) = chosen.filter(|n| !n.is_empty()) else {
                        app.status = tr!("沒有符合的 playbook");
                        return Ok(());
                    };
                    app.picker = None;
                    app.mode = Mode::Command;
                    app.input = format!("playbook {name} ");
                    app.input_cursor = app.input.chars().count();
                    app.status = tr!("輸入目標需求後按 Enter 啟動 playbook {name}", name = name);
                }
                PickStage::Resume => {
                    let Some(id) = chosen else {
                        return Ok(());
                    };
                    let found = app
                        .state
                        .sessions
                        .get(&id)
                        .or_else(|| app.state.archived_sessions.get(&id))
                        .or_else(|| {
                            app.hosts.values().find_map(|h| {
                                h.state
                                    .sessions
                                    .get(&id)
                                    .or_else(|| h.state.archived_sessions.get(&id))
                            })
                        })
                        .cloned();
                    if let Some(s) = found {
                        let label = s.label.clone();
                        if !s.status.is_live() && s.native_id.is_some() {
                            if let Some((text, attachments)) = app.pending_prompt_draft.take() {
                                send_resume_with_prompt(app, writer, id, text, attachments)?;
                            } else {
                                app.select_or_pend(&id);
                                crate::send(
                                    writer,
                                    &ertangle_protocol::Request::Resume {
                                        session: id,
                                        prompt: None,
                                    },
                                )?;
                                app.status = tr!("已要求接續 {label}", label = label);
                            }
                            app.picker = None;
                            app.mode = Mode::Normal;
                        } else if !s.status.is_live() {
                            app.status = tr!(
                                "{label} 沒有可接續的對話 id (PTY、舊版 log，或 agent 沒回報就結束)",
                                label = label
                            );
                        }
                    }
                }
                // 問題選擇器由 `handle_question_key` 處理。
                PickStage::Question { .. } => {}
                PickStage::ConfirmResumePrompt {
                    session,
                    status,
                    resumable: _,
                    text,
                    attachments,
                    ..
                } => {
                    let action = chosen.as_deref().unwrap_or("cancel");
                    app.picker = None;
                    if action == "confirm" {
                        app.mode = Mode::Normal;
                        if status == SessionStatus::Sleeping {
                            crate::send(
                                writer,
                                &ertangle_protocol::Request::Wake {
                                    session: session.clone(),
                                },
                            )?;
                            let n = attachments.len();
                            crate::send(
                                writer,
                                &ertangle_protocol::Request::Prompt {
                                    session,
                                    text,
                                    attachments,
                                    queue: false,
                                },
                            )?;
                            app.status = if n > 0 {
                                tr!("已喚醒並送出 prompt (含 {n} 個附件)", n = n)
                            } else {
                                tr!("已喚醒並送出 prompt")
                            };
                        } else {
                            send_resume_with_prompt(app, writer, session, text, attachments)?;
                        }
                    } else {
                        restore_prompt_draft(app, text);
                    }
                }
                PickStage::NoSessionPrompt {
                    text, attachments, ..
                } => {
                    let action = chosen.as_deref().unwrap_or("cancel");
                    app.picker = None;
                    match action {
                        "create" => {
                            app.pending_prompt_draft = Some((text, attachments));
                            start_spawn_picker(app);
                        }
                        "resume" => {
                            app.pending_prompt_draft = Some((text, attachments));
                            start_resume_picker(app);
                        }
                        _ => restore_prompt_draft(app, text),
                    }
                }
                PickStage::Project => {
                    let chosen_path = if chosen.is_some() {
                        chosen
                    } else if !custom.is_empty() {
                        Some(custom)
                    } else {
                        None
                    };
                    app.picker = None;
                    app.mode = Mode::Normal;
                    if let Some(target) = chosen_path {
                        if let Err(err) = app.set_active_project(std::path::Path::new(&target)) {
                            app.status = err;
                        }
                    }
                }
                PickStage::Directory { current } => {
                    let value = chosen.unwrap_or_default();
                    // 直接輸入絕對路徑時優先採用輸入值，避免與「使用目前資料夾」的說明文字意外相符。
                    let typed = if custom == "~" {
                        home_dir().unwrap_or_else(|| PathBuf::from(&custom))
                    } else if let Some(rest) = custom.strip_prefix("~/") {
                        home_dir()
                            .map(|home| home.join(rest))
                            .unwrap_or_else(|| PathBuf::from(&custom))
                    } else {
                        PathBuf::from(&custom)
                    };
                    let typed_dir =
                        if (custom.starts_with('/') || custom == "~" || custom.starts_with("~/"))
                            && typed.is_dir()
                        {
                            Some(typed)
                        } else {
                            None
                        };
                    if let Some(target) = typed_dir {
                        start_directory_picker(app, target);
                    } else if value == SELECT_VALUE {
                        app.picker = None;
                        app.mode = Mode::Normal;
                        if let Err(err) = app.set_active_project(&current) {
                            app.status = err;
                        } else {
                            app.status = tr!("已切換專案：{path}", path = current.display());
                        }
                    } else if value == HOME_VALUE {
                        match home_dir() {
                            Some(home) => start_directory_picker(app, home),
                            None => app.status = tr!("找不到家目錄"),
                        }
                    } else if value == PARENT_VALUE {
                        let parent = current
                            .parent()
                            .filter(|parent| *parent != current.as_path())
                            .map(Path::to_path_buf)
                            .unwrap_or(current);
                        start_directory_picker(app, parent);
                    } else if !value.is_empty() {
                        start_directory_picker(app, current.join(value));
                    } else if !custom.is_empty() {
                        let candidate = current.join(&custom);
                        if candidate.is_dir() {
                            start_directory_picker(app, candidate);
                        } else {
                            app.status = tr!("找不到資料夾：{path}", path = custom);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
