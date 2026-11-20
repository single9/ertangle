//! `termade` — 單一執行檔，多種模式。
//!
//! * `termade` / `termade tui`：確保 daemon 在跑，然後 attach TUI。
//! * `termade daemon [--listen host:port] [--token secret]`：前景執行 daemon (支援 M5 遠端連線)。
//! * `termade mcp`：stdio MCP server (供 agent 子處理程序呼叫協作原語)。
//! * M2 協作：`whoami`、`peers`/`sessions`、`spawn`、`prompt`、`send`、`inbox`、`wait`、`ask`、`tasks`、`task`、`task-update`、`status`、`artifact`
//! * M3 工作區與審查：`worktrees`、`diff`、`review`、`merge`、`pr`、`gc`
//! * M4 打磨：`sleep`、`wake`、`cost`、`replay`、`export`、`playbook`
//! * Shell 自動完成：`completions <bash|zsh|fish>` (隱藏的 `__complete sessions` 供自動完成腳本查 session)

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
use cmd::{
    bus::*, completions::*, cost::*, daemon::*, export::*, session::*, skill::*, task::*,
    worktree::*,
};

#[derive(Parser)]
#[command(
    name = "termade",
    version,
    about = "termade — 終端機原生的多 agent 開發環境"
)]
struct Cli {
    /// 連線遠端 daemon 位址 (例如 127.0.0.1:7420；亦可透過環境變數 TERMADE_REMOTE 設定)
    #[arg(long, global = true)]
    remote: Option<String>,
    /// 遠端認證 token (亦可透過環境變數 TERMADE_TOKEN 設定)
    #[arg(long, global = true)]
    token: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 啟動 TUI (預設行為)
    Tui,
    /// 以前景方式執行 daemon
    Daemon {
        /// 同時綁定 TCP 位址供遠端連線 (例如 127.0.0.1:7420，建議搭配 SSH 轉發；需 token)
        #[arg(long)]
        listen: Option<String>,
        /// 啟動 Web 版 HTTP/WebSocket 介面 (例如 tailscale、127.0.0.1:7421 或 8080)
        #[arg(long)]
        web: Option<String>,
        /// Web 版自訂連接埠 (預設 7421)
        #[arg(long)]
        web_port: Option<u16>,
        /// 遠端連線認證用的 token (未指定則讀環境變數 TERMADE_TOKEN；為安全建議搭配 SSH 轉發)
        #[arg(long)]
        token: Option<String>,
        /// 從檔案讀取 token (權限需為 0600 或 0400；避免 token 出現在 process list)
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
        /// 允許在未走 SSH 轉發下綁定非 loopback 位址 (危險：傳輸為明文，僅供專用私有網路)
        #[arg(long)]
        insecure_listen_public: bool,
    },
    /// 顯示遠端連線資訊 (Tailscale 位址、Token 路徑與連線指令)
    RemoteInfo {
        #[arg(long)]
        json: bool,
    },
    /// stdio MCP server 模式 (供 agent 子處理程序設定為 MCP 工具)
    Mcp {
        /// 此 MCP server 代表的 session id (預設讀 TERMADE_SESSION 環境變數)
        #[arg(long)]
        session: Option<String>,
        /// daemon socket 路徑 (預設讀取 TERMADE_SOCKET 環境變數或標準 runtime 路徑)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// 顯示目前環境變數指定的 session 資訊
    Whoami {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 列出所有 session
    #[command(alias = "peers")]
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// 建立新的 agent session
    Spawn {
        /// Agent 種類：opencode / claude / codex / antigravity；可加 `:model` (例如 claude:opus)
        #[arg(long, value_parser = KindParser)]
        kind: Option<String>,
        /// 啟動後立刻送出的 prompt
        #[arg(long)]
        prompt: Option<String>,
        /// 工作目錄 (預設為目前目錄)
        #[arg(long)]
        cwd: Option<String>,
        /// 易讀標籤 (例如 worker/backend；未指定時自動產生)
        #[arg(long)]
        label: Option<String>,
        /// 角色：coordinator / worker / reviewer / tester (省略時無 parent 依 [defaults] role，有 parent 則 worker)
        #[arg(long)]
        role: Option<String>,
        /// 在獨立的 git worktree 中執行 (M3)
        #[arg(long)]
        worktree: bool,
        /// 傳輸通道：auto (預設)/ structured / pty
        #[arg(long, default_value = "auto", value_parser = transport_values())]
        transport: String,
        /// 權限政策：ask / auto / bypass (省略則依 config.toml，預設 auto)
        #[arg(long, value_parser = permission_values(), ignore_case = true)]
        permission: Option<String>,
        /// 父 session id 或標籤 (未指定時若有 TERMADE_SESSION 則自動繼承)
        #[arg(long)]
        parent: Option<String>,
        /// 明確指定 model (優先於 kind 的 `:model` 與設定檔)
        #[arg(long)]
        model: Option<String>,
        /// 推理強度：low / medium / high / xhigh / max (省略則依 config.toml)
        #[arg(long, value_parser = EFFORT_VALUES)]
        effort: Option<String>,
        /// 此 session 的成本上限 (美元，例如 2.50)
        #[arg(long)]
        budget: Option<f64>,
        /// 附加檔案或圖片到初始 prompt (可重複)
        #[arg(short = 'a', long = "attach")]
        attach: Vec<String>,
        /// 附加給底層 agent 的命令列參數
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// 送出 prompt 給指定 session
    Prompt {
        session: String,
        /// 附加檔案或圖片 (可重複)
        #[arg(short = 'a', long = "attach")]
        attach: Vec<String>,
        /// 若 session 忙碌，先排入待送佇列，等閒下來再自動送出
        #[arg(short = 'q', long = "queue")]
        queue: bool,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// 查看或清空指定 session 的待送 prompt 佇列
    Queue {
        session: String,
        /// 清空待送佇列
        #[arg(long)]
        clear: bool,
    },
    /// 中斷正在執行的 turn
    Cancel { session: String },
    /// 強制結束 agent 子處理程序
    Kill { session: String },
    /// 檢視指定 session 或所有 session 的背景子處理程序與執行中工具
    Ps {
        /// 指定 session id 或標籤 (未指定則列出所有 session)
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 終止指定 session 的背景子處理程序 (不影響 agent 本體)
    BgKill {
        /// 目標 session id 或標籤
        session: String,
        /// 要終止的背景處理程序 PID (未指定且有 --all 則終止全部)
        #[arg(long)]
        pid: Option<u32>,
        /// 終止該 session 底下所有背景子處理程序
        #[arg(long)]
        all: bool,
        /// 強制終止 (SIGKILL)
        #[arg(long)]
        force: bool,
    },
    /// 接續已結束或已移除的歷史 session：用原本的對話繼續 (session id 不變)；搭配 --list 列出可接續的歷史 session
    Resume {
        /// 要接續的 session id、label 或原生對話 id；搭配 --list 可省略
        session: Option<String>,
        /// 列出所有可接續的已結束與歷史封存 session
        #[arg(long, short = 'l')]
        list: bool,
        /// 接續後立刻送出的 prompt (可省略)
        text: Vec<String>,
    },
    /// 讓 session 休眠 (停止處理程序但保留對話紀錄，可隨時喚醒)
    Sleep { session: String },
    /// 喚醒已休眠的 session
    Wake { session: String },
    /// 執行期切換 session 的 model 與／或 effort (下一個回合起生效)
    Model {
        session: String,
        /// 新的 model；省略則不變
        model: Option<String>,
        /// 新的推理強度 (low|medium|high|xhigh|max)
        #[arg(long)]
        effort: Option<String>,
    },
    /// 把 session 的工作交接給另一個 agent (額度用完時)：沿用同一個工作目錄，附上交接簡報與 diff
    Handoff {
        session: String,
        /// 接手的 agent，可加 `:model`，例如 codex、claude:sonnet
        #[arg(long, value_parser = KindParser)]
        to: String,
        #[arg(long, value_parser = EFFORT_VALUES)]
        effort: Option<String>,
        /// 補充給接手者的說明
        #[arg(long)]
        note: Option<String>,
        /// 變更角色：coordinator / worker / reviewer
        #[arg(long)]
        role: Option<String>,
        /// 變更父 session：<session_id|none|keep>
        #[arg(long)]
        parent: Option<String>,
        /// 強制執行 (例如在仍有執行中子 session 時降級 coordinator)
        #[arg(long)]
        force: bool,
    },
    /// 從清單移除已結束的 session (有 worktree 分支時需已合併，或加 --force)
    Rm {
        session: String,
        #[arg(long)]
        force: bool,
    },
    /// 透過匯流排傳送訊息給其他 session
    Send {
        /// 收件者：session id、標籤 (例如 worker/b)、或 @id / @label
        #[arg(long, conflicts_with_all = ["channel", "broadcast"])]
        to: Option<String>,
        /// 頻道名稱 (例如 backend；會廣播給所有同角色或訂閱者)
        #[arg(long, conflicts_with_all = ["to", "broadcast"])]
        channel: Option<String>,
        /// 廣播給所有 session
        #[arg(long, conflicts_with_all = ["to", "channel"])]
        broadcast: bool,
        /// 寄件者 session id (預設讀 TERMADE_SESSION 環境變數)
        #[arg(long)]
        from: Option<String>,
        /// 訊息種類：chat (預設)/ handoff / question / answer / status / artifact
        #[arg(long, default_value = "chat")]
        kind: String,
        /// 回覆的訊息 id (形成對話串)
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// 查看目前 session 的收件匣
    Inbox {
        #[arg(long)]
        session: Option<String>,
        /// 包含已讀訊息 (預設只顯示新進未讀)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// 阻塞等待下一則發給自己的訊息
    Wait {
        #[arg(long)]
        session: Option<String>,
        /// 只等待來自特定寄件者的訊息
        #[arg(long)]
        from: Option<String>,
        /// 逾時毫秒數 (預設 60000 = 60 秒)
        #[arg(long, default_value = "60000")]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// 同步問答：送出 question 訊息並阻塞等待對方回覆 answer
    Ask {
        /// 對象 session id 或標籤
        peer: String,
        #[arg(long)]
        session: Option<String>,
        /// 逾時毫秒數 (預設 120000 = 2 分鐘)
        #[arg(long, default_value = "120000")]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
        #[arg(required = true)]
        question: Vec<String>,
    },
    /// 列出所有協作任務
    Tasks {
        #[arg(long)]
        json: bool,
    },
    /// 建立新任務 (M2 看板)
    Task {
        title: String,
        #[arg(long)]
        detail: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        depends_on: Vec<String>,
    },
    /// 更新任務狀態或指派對象
    TaskUpdate {
        id: String,
        /// 新狀態：todo / doing / done / blocked
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        detail: Option<String>,
    },
    /// 廣播自己的最新狀態摘要
    Status {
        #[arg(long)]
        session: Option<String>,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// 分享重要產出物 (檔案、PR 連結、測試報告等)
    Artifact {
        path: String,
        /// 種類：code / pr / doc / test / run (預設 code)
        #[arg(long, default_value = "code")]
        kind: String,
        /// 說明備註
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// 列出所有由 termade 管理的 git worktree (M3)
    Worktrees {
        #[arg(long)]
        json: bool,
    },
    /// 檢視指定 session 的 worktree git diff (M3)
    Diff { session: String },
    /// 對指定 session 的程式碼留下審查註解並送回該 agent (M3)
    Review {
        session: String,
        /// 檔案與行號位置 (例如 `src/lib.rs:42`)
        #[arg(long)]
        location: String,
        #[arg(required = true)]
        comment: Vec<String>,
    },
    /// 將指定 session 的 worktree 合併回主分支 (M3)
    Merge {
        session: String,
        /// 僅乾跑：不改動 repo，回報會 commit 的檔案與是否可快轉
        #[arg(long)]
        dry_run: bool,
        /// 若發生衝突，自動保留標記於 worktree 並委派 agent 解衝突
        #[arg(long)]
        resolve: bool,
        /// 合併策略：merge | squash | rebase
        #[arg(long, value_parser = ["merge", "squash", "rebase"])]
        strategy: Option<String>,
    },
    /// 為指定 session 的 worktree 建立 GitHub PR (M3)
    Pr {
        session: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
    },
    /// 清理已結束或指定的 worktree (M3)
    Gc {
        /// 只清理特定 session 的 worktree；未指定則清理所有已結束者
        #[arg(long)]
        session: Option<String>,
        /// 強制刪除 (即使有未 commit 變更或未合併分支)
        #[arg(long)]
        force: bool,
    },
    /// 顯示各 agent 帳號的額度 (5 小時／7 天視窗與重置時間)
    Quota {
        #[arg(long)]
        json: bool,
    },
    /// 查看各 session 與整體 token / USD 成本統計
    Cost {
        #[arg(long)]
        json: bool,
    },
    /// 設定或調高指定 session 的成本上限 (美元)
    Budget {
        session: String,
        /// 新的成本上限 (美元，例如 10)
        amount: f64,
    },
    /// 重播 event log 並印出歷史事件
    Replay {
        /// 只重播序號大於此值的事件
        #[arg(long, default_value = "0")]
        since: u64,
        /// 指定 events.jsonl 路徑 (預設為 termade 自己的 log)
        #[arg(long)]
        file: Option<PathBuf>,
        /// 包含已輪替的舊紀錄檔 (events.jsonl.1 等；預設只讀目前活躍檔)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// 依照內建或自訂的團隊範本一次啟動多 Agent 協作團隊
    Playbook {
        /// 範本名稱：coord-review / tdd-pair / dual-review，或 config.toml 自訂的名稱
        name: String,
        /// 寫入各成員 prompt 中 `{{goal}}` 的目標需求
        goal: Vec<String>,
    },
    /// 把 session (與其子 Session 樹)匯出為 Markdown 協作報告
    Export {
        /// Session id 或標籤
        session: String,
        /// 包含整個子 Session 階層
        #[arg(long)]
        tree: bool,
        /// 在報告末尾附上該 session worktree 的 git diff
        #[arg(long)]
        diff: bool,
        /// 輸出檔案路徑 (省略則印到 stdout)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// 安裝／移除／顯示內嵌的 termade-ade skill
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    /// 關閉常駐的 daemon 與所有受管 agent 子處理程序
    Shutdown,
    /// 產生 shell 自動完成腳本並印到 stdout (bash / zsh / fish)
    Completions { shell: clap_complete::Shell },
    /// 印出所有說明文字 (供翻譯抽取用，不會啟動 daemon)
    #[command(name = "__help-strings", hide = true)]
    HelpStrings,
    /// 自動完成腳本用的查詢 (不會啟動 daemon)
    #[command(name = "__complete", hide = true)]
    Complete {
        #[arg(value_parser = ["sessions"])]
        what: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SkillCmd {
    /// 把 termade-ade skill 安裝到 agent 的 skills 目錄
    Install {
        /// 目標 agent：claude (.claude/skills)、agents (.agents/skills)、codex (.codex/skills)
        #[arg(long, default_value = "claude", value_parser = ["claude", "agents", "codex"])]
        target: String,
        /// 安裝到家目錄 (所有專案共用)
        #[arg(long, conflicts_with = "project")]
        global: bool,
        /// 安裝到目前專案 (預設)
        #[arg(long)]
        project: bool,
    },
    /// 移除已安裝的 termade-ade skill
    Uninstall {
        /// 目標 agent：claude、agents、codex
        #[arg(long, default_value = "claude", value_parser = ["claude", "agents", "codex"])]
        target: String,
        /// 從家目錄移除
        #[arg(long, conflicts_with = "project")]
        global: bool,
        /// 從目前專案移除 (預設)
        #[arg(long)]
        project: bool,
    },
    /// 印出內嵌的 SKILL.md
    Show,
}

/// 依全域設定的 `[tui] language` 與環境變數決定介面語言。daemon 也走這裡，
/// 所以 daemon 回給使用者的訊息跟 daemon 啟動時的語言一致。
fn init_language() {
    let path = termade_daemon::config_dir().join("config.toml");
    let cfg = termade_daemon::config::Config::load_file(&path);
    let lang = termade_core::i18n::resolve(cfg.tui.language.as_deref());
    if let Err(e) = termade_core::i18n::set_language(lang) {
        tracing::warn!("載入翻譯失敗，改用繁體中文：{e}");
    }
}

fn main() -> Result<()> {
    init_tracing();
    init_language();
    let cli = {
        use clap::{CommandFactory, FromArgMatches};
        let cmd = if termade_core::i18n::current() == termade_core::i18n::Language::ZhTw {
            Cli::command()
        } else {
            cmd::helptext::localize(Cli::command())
        };
        Cli::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit())
    };
    if let Some(r) = &cli.remote {
        std::env::set_var("TERMADE_REMOTE", r);
    }
    // daemon 子命令自己處理 --token (含棄用警告)，避免與全域 --token 混淆。
    if let Some(t) = &cli.token {
        if !matches!(cli.cmd, Some(Cmd::Daemon { .. })) {
            std::env::set_var("TERMADE_TOKEN", t);
        }
    }

    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Tui => termade_tui::run(),
        Cmd::Daemon {
            listen,
            web,
            web_port,
            token,
            token_file,
            insecure_listen_public,
        } => cmd_daemon(
            listen,
            web,
            web_port,
            token,
            token_file,
            insecure_listen_public,
        ),
        Cmd::RemoteInfo { json } => cmd_remote_info(json),
        Cmd::Mcp { session, socket } => termade_daemon::mcp::run_with_socket(session, socket),
        Cmd::Whoami { session, json } => cmd_whoami(session, json),
        Cmd::Sessions { json } => cmd_sessions(json),
        Cmd::Spawn {
            kind,
            prompt,
            cwd,
            label,
            role,
            worktree,
            transport,
            permission,
            parent,
            model,
            effort,
            budget,
            attach,
            extra,
        } => cmd_spawn(
            kind, prompt, cwd, label, role, worktree, transport, permission, parent, model, effort,
            budget, attach, extra,
        ),
        Cmd::Prompt {
            session,
            attach,
            queue,
            text,
        } => cmd_prompt(session, text, attach, queue),
        Cmd::Queue { session, clear } => cmd_queue(session, clear),
        Cmd::Cancel { session } => cmd_cancel(session),
        Cmd::Kill { session } => cmd_kill(session),
        Cmd::Ps { session, json } => cmd_ps(session, json),
        Cmd::BgKill {
            session,
            pid,
            all,
            force,
        } => cmd_bg_kill(session, pid, all, force),
        Cmd::Resume {
            session,
            list,
            text,
        } => cmd_resume(session, list, text),
        Cmd::Sleep { session } => cmd_sleep(session),
        Cmd::Wake { session } => cmd_wake(session),
        Cmd::Model {
            session,
            model,
            effort,
        } => cmd_model(session, model, effort),
        Cmd::Rm { session, force } => cmd_rm(session, force),
        Cmd::Handoff {
            session,
            to,
            effort,
            note,
            role,
            parent,
            force,
        } => cmd_handoff(session, to, effort, note, role, parent, force),
        Cmd::Send {
            to,
            channel,
            broadcast,
            from,
            kind,
            reply_to,
            body,
        } => cmd_send(to, channel, broadcast, from, kind, reply_to, body),
        Cmd::Inbox { session, all, json } => cmd_inbox(session, !all, json),
        Cmd::Wait {
            session,
            from,
            timeout_ms,
            json,
        } => cmd_wait(session, from, timeout_ms, json),
        Cmd::Ask {
            peer,
            session,
            timeout_ms,
            json,
            question,
        } => cmd_ask(peer, session, timeout_ms, json, question),
        Cmd::Tasks { json } => cmd_tasks(json),
        Cmd::Task {
            title,
            detail,
            assignee,
            depends_on,
        } => cmd_task_create(title, detail, assignee, depends_on),
        Cmd::TaskUpdate {
            id,
            status,
            assignee,
            detail,
        } => cmd_task_update(id, status, assignee, detail),
        Cmd::Status { session, text } => cmd_status(session, text),
        Cmd::Artifact {
            path,
            kind,
            note,
            session,
        } => cmd_artifact(path, kind, note, session),
        Cmd::Worktrees { json } => cmd_worktrees(json),
        Cmd::Diff { session } => cmd_diff(session),
        Cmd::Review {
            session,
            location,
            comment,
        } => cmd_review(session, location, comment),
        Cmd::Merge {
            session,
            dry_run,
            resolve,
            strategy,
        } => {
            let strategy = strategy
                .map(|s| s.parse::<termade_protocol::MergeStrategy>())
                .transpose()
                .map_err(anyhow::Error::msg)?;
            cmd_merge(session, dry_run, resolve, strategy)
        }
        Cmd::Pr {
            session,
            title,
            body,
        } => cmd_pr(session, title, body),
        Cmd::Gc { session, force } => cmd_gc(session, force),
        Cmd::Cost { json } => cmd_cost(json),
        Cmd::Budget { session, amount } => cmd_budget(session, amount),
        Cmd::Quota { json } => cmd_quota(json),
        Cmd::Replay {
            since,
            file,
            all,
            json,
        } => cmd_replay(since, file, all, json),
        Cmd::Playbook { name, goal } => cmd_playbook(name, goal),
        Cmd::Export {
            session,
            tree,
            diff,
            output,
        } => cmd_export(session, tree, diff, output),
        Cmd::Skill { cmd } => cmd_skill(cmd),
        Cmd::Shutdown => cmd_shutdown(),
        Cmd::Completions { shell } => cmd_completions(shell),
        Cmd::Complete { .. } => cmd_complete_sessions(),
        Cmd::HelpStrings => {
            use clap::CommandFactory;
            cmd::helptext::dump(&Cli::command());
            Ok(())
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("TERMADE_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
