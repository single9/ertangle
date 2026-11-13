//! 說明面板：`?`、`:help` 或 `Space ?` 開啟，列出所有按鍵與命令。

use crate::app::{App, Mode};
use crossterm::event::{KeyCode, KeyEvent};

/// 說明內容：(章節標題, [(按鍵, 說明)])。
pub(crate) const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "常用",
        &[
            ("Space", "選單（s session、g git、v 檢視、m 訊息）"),
            (":", "命令列（輸入時會提示，Tab 補全）；也可用 /"),
            ("n", "新增 agent：選 agent → model → 輸入 prompt"),
            (
                "i / Enter",
                "對選取的 session 輸入 prompt（也可 @peer、#channel 傳訊息）",
            ),
            ("j / k", "選取下一個／上一個 session"),
            ("Ctrl-K", "依關鍵字跳轉到 session"),
            ("?", "這個說明面板"),
            ("q / Q", "離開（保留 daemon）／關閉 daemon 並離開"),
        ],
    ),
    (
        "prompt 輸入",
        &[
            (
                "@檔名",
                "附加檔案或圖片；會在專案內模糊搜尋，↑↓ 選、Tab 採用",
            ),
            ("Ctrl-V", "貼上剪貼簿圖片（需 wl-paste 或 xclip）"),
            ("拖曳檔案", "把檔案拖進終端機會自動轉成 @路徑"),
            ("Ctrl-W", "（spawn 時）切換是否使用獨立 git worktree"),
            ("Ctrl-E", "（spawn 時）切換權限 ask／auto／bypass"),
            ("Ctrl-F", "（spawn 時）切換 effort；預設不指定"),
        ],
    ),
    (
        "Session",
        &[
            ("x", "中斷目前回合"),
            ("K", "結束 session"),
            ("z", "休眠／喚醒"),
            ("a / A / d", "審批：允許／一律允許／拒絕"),
            ("Space s h", "交接給其他 agent（額度用完時）"),
            ("Space s r", "接續已結束的 session：同一個 agent 接回原對話"),
            ("Space s x", "移除已結束的 session；Space s X 全部清除"),
        ],
    ),
    (
        "檢視",
        &[
            ("Tab", "切換 Overview／Diff／Worktrees／Tasks"),
            ("j k / Enter", "（Tasks 視圖）選任務／跳到負責人的 session"),
            ("g", "Diff 檢視"),
            ("L", "切換版面：三欄／兩欄／分頁"),
            ("b", "顯示／隱藏 Bus 浮層"),
            ("t / T", "原始文字／Markdown；顯示／隱藏思考"),
            ("O", "展開／摺疊工具呼叫（預設摺疊）"),
            ("↑ / ↓", "（輸入 prompt 或命令時）回顧歷史輸入"),
            ("P", "只看本專案／顯示全部專案的 session"),
            (
                "Ctrl-J / Shift-Enter",
                "（輸入 prompt 時）換行；輸入框隨行數長高，貼上保留換行",
            ),
            (
                "Alt-Enter",
                "（輸入 prompt 時）排隊，session 閒下來自動送出",
            ),
            ("PgUp / PgDn", "捲動 transcript；End 回到最新"),
            (
                "f",
                "搜尋逐字稿或 diff（邊打邊找）；Enter／↓ 下一個、↑ 上一個、Esc 離開",
            ),
            (
                "f 的範圍",
                "只找畫面上顯示的內容：摺疊的工具、隱藏的思考、超過 4000 行的舊內容找不到",
            ),
            ("拖曳 / Y / F2", "選取複製；複製目前畫面；切換滑鼠擷取"),
        ],
    ),
    (
        "Diff 與 git",
        &[
            ("j k { }", "移動游標／跳到上一個、下一個 hunk"),
            ("c", "對游標所在行寫審查註解，送回該 agent"),
            ("M", "合併選取 session 的 worktree；:merge --dry-run 先看"),
            ("G", "清理已合併的 worktree"),
        ],
    ),
    (
        "命令（: 開頭）",
        &[
            (
                ":spawn",
                "啟動 agent，如 :spawn claude:opus 做某事；不帶參數開選擇器",
            ),
            (":handoff", "交接給另一個 agent，如 :handoff codex 補充說明"),
            (
                ":resume",
                "接續已結束的 session（程序死掉或 daemon 重啟後）；可接 prompt",
            ),
            (":rm :clean", "移除已結束的 session／全部清除"),
            (":kill :cancel", "結束／中斷選取的 session"),
            (
                ":queue :unqueue",
                "排隊 prompt（查看／clear）／取消最後一則放回輸入框",
            ),
            (":quota", "各 agent 帳號額度與重置時間"),
            (
                ":budget",
                "設定或調高選取 session 的成本上限，如 :budget 10",
            ),
            (":layout", "切換版面；:raw、:thoughts、:bus 切換顯示"),
            (
                ":notify",
                "on|off 開關通知（回合結束、等待審批、失敗、額度用完）；test 試發；方式在 config.toml [tui] notify",
            ),
            (":find", "搜尋逐字稿或 diff，如 :find error（同 f）"),
            (":task :broadcast", "新增任務／廣播訊息"),
            (
                ":tasks :task-status",
                "任務視圖／改選取任務的狀態（todo doing blocked review done）",
            ),
        ],
    ),
    (
        "畫面上的標記",
        &[
            ("ctx 4%", "該 session 的 context 用量；≥80% 黃、≥92% 紅"),
            (
                "$1.20/$5",
                "該 session 的成本與上限；達警告門檻黃、超過上限紅",
            ),
            ("claude:5h 41%", "帳號額度最吃緊的視窗；≥70% 黃、≥90% 紅"),
            (
                "[claude:opus !bypass]",
                "agent:model；!bypass 表示跳過所有權限檢查",
            ),
            ("● ○ ✗ ✓", "忙碌／閒置／失敗／結束"),
            ("⧗a1b2c3", "（Tasks 視圖）任務的前置任務；黃色是還沒完成的"),
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
        out.push((true, (*title).to_string(), String::new()));
        for (k, d) in *rows {
            out.push((false, (*k).to_string(), (*d).to_string()));
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
