//! 說明面板：`?`、`:help` 或 `Space ?` 開啟，列出所有按鍵與命令。

use crate::app::{App, Mode};
use crossterm::event::{KeyCode, KeyEvent};
use termade_core::{tr_dyn, tr_noop};

/// 說明內容：(章節標題, [(按鍵, 說明)])。中文以 `tr_noop!` 標記，`help_lines` 顯示時翻譯。
pub(crate) const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        tr_noop!("常用"),
        &[
            ("Space", tr_noop!("選單 (s session、g git、v 檢視、m 訊息)")),
            (":", tr_noop!("命令列 (輸入時會提示，Tab 補全)；也可用 /")),
            ("n", tr_noop!("新增 agent：選 agent → model → 輸入 prompt")),
            (
                "i / Enter",
                tr_noop!("對選取的 session 輸入 prompt (也可 @peer、#channel 傳訊息)"),
            ),
            ("j / k", tr_noop!("選取下一個／上一個 session")),
            ("Ctrl-K", tr_noop!("依關鍵字跳轉到 session")),
            ("?", tr_noop!("這個說明面板")),
            ("Space ,", tr_noop!("開啟設定面板 (含 Web 版支援)")),
            ("Space w", tr_noop!("顯示 Web 版網址與 QR Code")),
            ("q / Q", tr_noop!("離開 (保留 daemon)／關閉 daemon 並離開")),
        ],
    ),
    (
        tr_noop!("prompt 輸入"),
        &[
            (
                tr_noop!("@檔名"),
                tr_noop!("附加檔案或圖片；會在專案內模糊搜尋，↑↓ 選、Tab 採用"),
            ),
            ("Ctrl-V", tr_noop!("貼上剪貼簿圖片 (需 wl-paste 或 xclip)")),
            (tr_noop!("拖曳檔案"), tr_noop!("把檔案拖進終端機會自動轉成 @路徑")),
            ("Ctrl-W", tr_noop!("(spawn 時)切換是否使用獨立 git worktree")),
            ("Ctrl-R", tr_noop!("(spawn 時)切換角色 worker／coordinator／reviewer")),
            ("Ctrl-E", tr_noop!("(spawn 時)切換權限 ask／auto／bypass")),
            ("Ctrl-F", tr_noop!("(spawn 時)切換 effort；預設不指定")),
        ],
    ),
    (
        "Session",
        &[
            ("x", tr_noop!("中斷目前回合")),
            ("K", tr_noop!("結束 session")),
            ("z", tr_noop!("休眠／喚醒")),
            ("a / A / d", tr_noop!("核准：允許／一律允許／拒絕")),
            ("Space s h", tr_noop!("交接給其他 agent (額度用完時)")),
            ("Space s r", tr_noop!("接續已結束的 session：同一個 agent 接回原對話")),
            ("Space s x", tr_noop!("移除已結束的 session；Space s X 全部清除")),
            (
                ":clean --force",
                tr_noop!("強制清除未合併或有變更的 worktree (會失去內容)"),
            ),
        ],
    ),
    (
        tr_noop!("檢視"),
        &[
            ("Tab", tr_noop!("切換 Overview／Diff／Worktrees／Tasks")),
            ("j k / Enter", tr_noop!("(Tasks 視圖)選任務／跳到負責人的 session")),
            ("g", tr_noop!("Diff 檢視")),
            ("L", tr_noop!("切換版面：三欄／兩欄／分頁")),
            ("b", tr_noop!("顯示／隱藏 Bus 浮層")),
            ("t / T", tr_noop!("原始文字／Markdown；顯示／隱藏思考")),
            ("O", tr_noop!("展開／摺疊工具呼叫 (預設摺疊)")),
            ("↑ / ↓", tr_noop!("(輸入 prompt 或命令時)回顧歷史輸入")),
            ("P", tr_noop!("只看本專案／顯示全部專案 (session、Bus、任務)")),
            (
                "Ctrl-J / Shift-Enter",
                tr_noop!("(輸入 prompt 時)換行；輸入框隨行數長高，貼上保留換行"),
            ),
            (
                "Alt-Enter",
                tr_noop!("(輸入 prompt 時)排隊，session 閒下來自動送出"),
            ),
            ("PgUp / PgDn", tr_noop!("捲動 transcript；End 回到最新")),
            (
                "f",
                tr_noop!("搜尋逐字稿或 diff (邊打邊找)；Enter／↓ 下一個、↑ 上一個、Esc 離開"),
            ),
            (
                tr_noop!("f 的範圍"),
                tr_noop!("只找畫面上顯示的內容：摺疊的工具、隱藏的思考、超過 4000 行的舊內容找不到"),
            ),
            (
                tr_noop!("拖曳 / Y / F2"),
                tr_noop!("選取複製；複製內容 (Transcript 預設最新訊息，可用 :copy N 指定)；切換滑鼠擷取"),
            ),
        ],
    ),
    (
        tr_noop!("Diff 與 git"),
        &[
            ("j k { }", tr_noop!("移動游標／跳到上一個、下一個 hunk")),
            ("( ) / < >", tr_noop!("跳到上一個、下一個檔案 (diff --git)")),
            ("Enter / v", tr_noop!("(Diff 視圖)摺疊／展開目前游標所在的檔案")),
            (
                "X / :discard",
                tr_noop!("(Diff 視圖)退回游標所在的 hunk 或整個檔案；再按一次 X 或 :discard! 確認"),
            ),
            ("c", tr_noop!("對游標所在行寫審查註解，送回該 agent")),
            (
                "M",
                tr_noop!("合併選取 session 的 worktree；:merge --dry-run 先看；:merge --resolve 委派解衝突"),
            ),
            ("G", tr_noop!("清理已合併的 worktree")),
        ],
    ),
    (
        tr_noop!("命令 (: 開頭)"),
        &[
            (
                ":spawn",
                tr_noop!("啟動 agent，如 :spawn --role coord claude:opus 做某事；不帶參數開選擇器"),
            ),
            (
                ":playbook",
                tr_noop!("啟動多 Agent 團隊範本，如 :playbook tdd-pair 完成登入功能；不帶參數開選擇器 (Space s b)"),
            ),
            (
                ":export",
                tr_noop!("匯出目前 session 樹為 Markdown 至 .termade/exports/ 並複製到剪貼簿；可指定路徑"),
            ),
            (":handoff", tr_noop!("交接給另一個 agent，如 :handoff codex 補充說明")),
            (
                ":resume",
                tr_noop!("接續已結束的 session (處理程序死掉或 daemon 重啟後)；可接 prompt"),
            ),
            (":rm :clean", tr_noop!("移除選取的 session／清除所有已結束的 session (含 worktree)")),
            (":copy [N]", tr_noop!("複製內容；Transcript 可指定第 N 則訊息 (預設最新)")),
            (":kill :cancel", tr_noop!("結束／中斷選取的 session")),
            (
                ":queue :unqueue",
                tr_noop!("排隊 prompt (查看／clear)／取消最後一則放回輸入框"),
            ),
            (":quota", tr_noop!("各 agent 帳號額度與重置時間")),
            (
                ":budget",
                tr_noop!("設定或調高選取 session 的成本上限，如 :budget 10"),
            ),
            (":layout", tr_noop!("切換版面；:raw、:thoughts、:bus 切換顯示")),
            (
                ":notify",
                tr_noop!("on|off 開關通知 (回合結束、等待核准、失敗、額度用完)；test 試發；方式在 config.toml [tui] notify"),
            ),
            (":find", tr_noop!("搜尋逐字稿或 diff，如 :find error (同 f)")),
            (":task :broadcast", tr_noop!("新增任務／廣播訊息")),
            (
                ":tasks :task-status",
                tr_noop!("任務視圖／改選取任務的狀態 (todo doing blocked review done)"),
            ),
        ],
    ),
    (
        tr_noop!("畫面上的標記"),
        &[
            ("ctx 4%", tr_noop!("該 session 的 context 用量；≥80% 黃、≥92% 紅")),
            (
                "$1.20/$5",
                tr_noop!("該 session 的成本與上限；達警告門檻黃、超過上限紅"),
            ),
            ("claude:5h 41%", tr_noop!("帳號額度最吃緊的視窗；≥70% 黃、≥90% 紅")),
            (
                "[claude:opus !bypass]",
                tr_noop!("agent:model；!bypass 表示跳過所有權限檢查"),
            ),
            ("● ○ ✗ ✓", tr_noop!("忙碌／閒置／失敗／結束")),
            ("⧗a1b2c3", tr_noop!("(Tasks 視圖)任務的前置任務；黃色是還沒完成的")),
        ],
    ),
];

/// 說明內容攤平成顯示行。
pub(crate) fn help_lines() -> Vec<(bool, String, String)> {
    let mut out = Vec::new();
    for (title, rows) in HELP {
        if !out.is_empty() {
            out.push((false, String::new(), String::new()));
        }
        out.push((true, tr_dyn!(title), String::new()));
        for (k, d) in *rows {
            out.push((false, tr_dyn!(k), tr_dyn!(d)));
        }
    }
    out
}

pub(crate) fn open_help(app: &mut App) {
    app.mode = Mode::Help;
    app.help_scroll = 0;
}

pub(crate) fn handle_help_key(app: &mut App, key: KeyEvent) {
    let total = help_lines().len();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => {
            app.mode = Mode::Normal;
        }
        KeyCode::Down | KeyCode::Char('j') => app.help_scroll = (app.help_scroll + 1).min(total),
        KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
        KeyCode::PageDown | KeyCode::Char(' ') => {
            app.help_scroll = (app.help_scroll + 10).min(total)
        }
        KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(10),
        KeyCode::Home => app.help_scroll = 0,
        _ => {}
    }
}
