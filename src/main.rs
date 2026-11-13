//! `ade` — 單一執行檔，多種模式。
//!
//! * `ade` / `ade tui`：確保 daemon 在跑，然後 attach TUI。
//! * `ade daemon [--listen host:port] [--token secret]`：前景執行 daemon（支援 M5 遠端連線）。
//! * `ade mcp`：stdio MCP server（供 agent 子程序呼叫協作原語）。
//! * M2 協作：`whoami`、`peers`/`sessions`、`spawn`、`prompt`、`send`、`inbox`、`wait`、`ask`、`tasks`、`task`、`task-update`、`status`、`artifact`
//! * M3 工作區與審查：`worktrees`、`diff`、`review`、`merge`、`pr`、`gc`
//! * M4 打磨：`sleep`、`wake`、`cost`、`replay`

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
use cmd::{bus::*, cost::*, daemon::*, session::*, task::*, worktree::*};

#[derive(Parser)]
#[command(
    name = "ade",
    version,
    about = "ade — 終端機原生的 Agent Development Environment"
)]
struct Cli {
    /// 連線遠端 daemon 位址（例如 127.0.0.1:7420；亦可透過環境變數 ADE_REMOTE 設定）
    #[arg(long, global = true)]
    remote: Option<String>,
    /// 遠端認證 token（亦可透過環境變數 ADE_TOKEN 設定）
    #[arg(long, global = true)]
    token: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 啟動 TUI（預設行為）
    Tui,
    /// 以前景方式執行 daemon
    Daemon {
        /// 同時綁定 TCP 位址供遠端連線（例如 127.0.0.1:7420，建議搭配 SSH 轉發；需 token）
        #[arg(long)]
        listen: Option<String>,
        /// 遠端連線認證用的 token（未指定則讀環境變數 ADE_TOKEN；為安全建議搭配 SSH 轉發）
        #[arg(long)]
        token: Option<String>,
        /// 從檔案讀取 token（權限需為 0600 或 0400；避免 token 出現在 process list）
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
        /// 允許在未走 SSH 轉發下綁定非 loopback 位址（危險：傳輸為明文，僅供專用私有網路）
        #[arg(long)]
        insecure_listen_public: bool,
    },
    /// stdio MCP server 模式（供 agent 子程序設定為 MCP 工具）
    Mcp {
        /// 此 MCP server 代表的 session id（預設讀 ADE_SESSION 環境變數）
        #[arg(long)]
        session: Option<String>,
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
        /// Agent 種類：opencode / claude / codex / antigravity；可加 `:model`（例如 claude:opus）
        #[arg(long, default_value = "opencode")]
        kind: String,
        /// 啟動後立刻送出的 prompt
        #[arg(long)]
        prompt: Option<String>,
        /// 工作目錄（預設為目前目錄）
        #[arg(long)]
        cwd: Option<String>,
        /// 易讀標籤（例如 worker/backend；未指定時自動產生）
        #[arg(long)]
        label: Option<String>,
        /// 角色：coordinator / worker / reviewer / tester（預設 worker）
        #[arg(long, default_value = "worker")]
        role: String,
        /// 在獨立的 git worktree 中執行（M3）
        #[arg(long)]
        worktree: bool,
        /// 傳輸通道：auto（預設）/ structured / pty
        #[arg(long, default_value = "auto")]
        transport: String,
        /// 權限政策：ask / auto / bypass（省略則依 config.toml，預設 auto）
        #[arg(long)]
        permission: Option<String>,
        /// 父 session id 或標籤（未指定時若有 ADE_SESSION 則自動繼承）
        #[arg(long)]
        parent: Option<String>,
        /// 明確指定 model（優先於 kind 的 `:model` 與設定檔）
        #[arg(long)]
        model: Option<String>,
        /// 推理強度：low / medium / high / xhigh / max（省略則依 config.toml）
        #[arg(long)]
        effort: Option<String>,
        /// 附加檔案或圖片到初始 prompt（可重複）
        #[arg(short = 'a', long = "attach")]
        attach: Vec<String>,
        /// 附加給底層 agent 的命令列參數
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// 送出 prompt 給指定 session
    Prompt {
        session: String,
        /// 附加檔案或圖片（可重複）
        #[arg(short = 'a', long = "attach")]
        attach: Vec<String>,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// 中斷正在執行的 turn
    Cancel { session: String },
    /// 強制結束 agent 子程序
    Kill { session: String },
    /// 讓 session 休眠（停止程序但保留對話紀錄，可隨時喚醒）
    Sleep { session: String },
    /// 喚醒已休眠的 session
    Wake { session: String },
    /// 把 session 的工作交接給另一個 agent（額度用完時）：沿用同一個工作目錄，附上交接簡報與 diff
    Handoff {
        session: String,
        /// 接手的 agent，可加 `:model`，例如 codex、claude:sonnet
        #[arg(long)]
        to: String,
        #[arg(long)]
        effort: Option<String>,
        /// 補充給接手者的說明
        #[arg(long)]
        note: Option<String>,
    },
    /// 從清單移除已結束的 session（有 worktree 分支時需已合併，或加 --force）
    Rm {
        session: String,
        #[arg(long)]
        force: bool,
    },
    /// 透過匯流排傳送訊息給其他 session
    Send {
        /// 收件者：session id、標籤（例如 worker/b）、或 @id / @label
        #[arg(long, conflicts_with_all = ["channel", "broadcast"])]
        to: Option<String>,
        /// 頻道名稱（例如 backend；會廣播給所有同角色或訂閱者）
        #[arg(long, conflicts_with_all = ["to", "broadcast"])]
        channel: Option<String>,
        /// 廣播給所有 session
        #[arg(long, conflicts_with_all = ["to", "channel"])]
        broadcast: bool,
        /// 寄件者 session id（預設讀 ADE_SESSION 環境變數）
        #[arg(long)]
        from: Option<String>,
        /// 訊息種類：chat（預設）/ handoff / question / answer / status / artifact
        #[arg(long, default_value = "chat")]
        kind: String,
        /// 回覆的訊息 id（形成對話線程）
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(required = true)]
        body: Vec<String>,
    },
    /// 查看目前 session 的收件匣
    Inbox {
        #[arg(long)]
        session: Option<String>,
        /// 包含已讀訊息（預設只顯示新進未讀）
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
        /// 逾時毫秒數（預設 60000 = 60 秒）
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
        /// 逾時毫秒數（預設 120000 = 2 分鐘）
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
    /// 建立新任務（M2 看板）
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
    /// 分享重要產出物（檔案、PR 連結、測試報告等）
    Artifact {
        path: String,
        /// 種類：code / pr / doc / test / run（預設 code）
        #[arg(long, default_value = "code")]
        kind: String,
        /// 說明備註
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// 列出所有由 ade 管理的 git worktree（M3）
    Worktrees {
        #[arg(long)]
        json: bool,
    },
    /// 檢視指定 session 的 worktree git diff（M3）
    Diff { session: String },
    /// 對指定 session 的程式碼留下審查註解並送回該 agent（M3）
    Review {
        session: String,
        /// 檔案與行號位置（例如 `src/lib.rs:42`）
        #[arg(long)]
        location: String,
        #[arg(required = true)]
        comment: Vec<String>,
    },
    /// 將指定 session 的 worktree 合併回主分支（M3）
    Merge {
        session: String,
        /// 僅乾跑：不改動 repo，回報會 commit 的檔案與是否可快轉
        #[arg(long)]
        dry_run: bool,
    },
    /// 為指定 session 的 worktree 建立 GitHub PR（M3）
    Pr {
        session: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
    },
    /// 清理已結束或指定的 worktree（M3）
    Gc {
        /// 只清理特定 session 的 worktree；未指定則清理所有已結束者
        #[arg(long)]
        session: Option<String>,
        /// 強制刪除（即使有未 commit 變更或未合併分支）
        #[arg(long)]
        force: bool,
    },
    /// 顯示各 agent 帳號的額度（5 小時／7 天視窗與重置時間）
    Quota {
        #[arg(long)]
        json: bool,
    },
    /// 查看各 session 與整體 token / USD 成本統計
    Cost {
        #[arg(long)]
        json: bool,
    },
    /// 重播 event log 並印出歷史事件
    Replay {
        /// 只重播序號大於此值的事件
        #[arg(long, default_value = "0")]
        since: u64,
        /// 指定 events.jsonl 路徑（預設為 ade 自己的 log）
        #[arg(long)]
        file: Option<PathBuf>,
        /// 包含已輪替的舊紀錄檔（events.jsonl.1 等；預設只讀目前活躍檔）
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// 關閉常駐的 daemon 與所有受管 agent 子程序
    Shutdown,
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    if let Some(r) = &cli.remote {
        std::env::set_var("ADE_REMOTE", r);
    }
    // daemon 子命令自己處理 --token（含棄用警告），避免與全域 --token 混淆。
    if let Some(t) = &cli.token {
        if !matches!(cli.cmd, Some(Cmd::Daemon { .. })) {
            std::env::set_var("ADE_TOKEN", t);
        }
    }

    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Tui => ade_tui::run(),
        Cmd::Daemon {
            listen,
            token,
            token_file,
            insecure_listen_public,
        } => cmd_daemon(listen, token, token_file, insecure_listen_public),
        Cmd::Mcp { session } => ade_daemon::mcp::run(session),
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
            attach,
            extra,
        } => cmd_spawn(
            kind, prompt, cwd, label, role, worktree, transport, permission, parent, model, effort,
            attach, extra,
        ),
        Cmd::Prompt {
            session,
            attach,
            text,
        } => cmd_prompt(session, text, attach),
        Cmd::Cancel { session } => cmd_cancel(session),
        Cmd::Kill { session } => cmd_kill(session),
        Cmd::Sleep { session } => cmd_sleep(session),
        Cmd::Wake { session } => cmd_wake(session),
        Cmd::Rm { session, force } => cmd_rm(session, force),
        Cmd::Handoff {
            session,
            to,
            effort,
            note,
        } => cmd_handoff(session, to, effort, note),
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
        Cmd::Merge { session, dry_run } => cmd_merge(session, dry_run),
        Cmd::Pr {
            session,
            title,
            body,
        } => cmd_pr(session, title, body),
        Cmd::Gc { session, force } => cmd_gc(session, force),
        Cmd::Cost { json } => cmd_cost(json),
        Cmd::Quota { json } => cmd_quota(json),
        Cmd::Replay {
            since,
            file,
            all,
            json,
        } => cmd_replay(since, file, all, json),
        Cmd::Shutdown => cmd_shutdown(),
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("ADE_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
