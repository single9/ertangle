//! `termade-adapter` — 把各家 agent 的正規化層。
//!
//! 每個 agent 的協定 (ACP、stream-json、app-server、PTY)都在這裡被翻譯成
//! 同一組 [`AgentEvent`]，所以 daemon 之上完全不需要知道底下是哪一家。
//!
//! 結構化 adapter (原生、零額外相依性)：
//! * [`acp`]：Agent Client Protocol — OpenCode (`opencode acp`)、Antigravity (`agy acp`)。
//! * [`claude`]：Claude Code 的 `--input-format/--output-format stream-json`。
//! * [`codex`]：Codex 的 `codex app-server`(stdio JSON-RPC)。
//!
//! 萬用後備：
//! * [`pty`]：PTY 直通，**對任何 CLI agent 都有效**。

pub mod acp;
pub mod ansi;
pub mod claude;
pub mod codex;
pub mod models;
pub mod pty;
pub mod stderr;
pub mod stream;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use termade_core::{
    AgentKind, Cost, Decision, PermissionPolicy, PermissionRequest, SessionId, SessionStatus,
};
use termade_protocol::Transport;
use tr::tr;

/// adapter 正規化後、往上送的事件。
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Status(SessionStatus),
    /// agent 的文字輸出 (可含換行)。
    Output(String),
    ToolCall {
        name: String,
        input: String,
        /// `None` = 進行中。
        ok: Option<bool>,
        output: Option<String>,
    },
    Permission(PermissionRequest),
    Cost(Cost),
    Model(String),
    /// agent 原生的斜線指令名稱 (不含 `/`)。
    Commands(Vec<String>),
    /// agent 原生的對話 id (接續用)。只有之後能用這個 id 接續時才會送出。
    NativeId(String),
    /// 額度用完／被限流。
    Limited(String),
    /// 設定 session 的狀態備註 (例如失敗原因)。
    Note(String),
    /// session 的 context 用量。
    Context {
        used: u64,
        window: Option<u64>,
    },
    /// 帳號額度狀態更新。
    Quota(termade_core::Quota),
    /// 處理程序結束。
    Exited(Option<i32>),
}

/// 啟動一個 session 所需的全部資訊。
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub id: SessionId,
    pub kind: AgentKind,
    pub cwd: PathBuf,
    pub transport: Transport,
    /// 自訂執行檔路徑或名稱 (覆寫預設 PATH 探測)。
    pub bin: Option<String>,
    /// 額外命令列參數。
    pub args: Vec<String>,
    /// 要使用的 model (目前僅 Claude 結構化模式支援)。
    pub model: Option<String>,
    /// 推理強度 (Claude `--effort`、Codex `model_reasoning_effort`、ACP configOption)。
    pub effort: Option<String>,
    /// Claude 專用：額外放行／禁止的工具。
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    /// 唯讀模式 (Claude 以 plan 模式達成)。
    pub read_only: bool,
    /// Codex 專用 sandbox 模式。
    pub sandbox: Option<String>,
    /// 額外環境變數。
    pub env: Vec<(String, String)>,
    /// 統一權限政策。
    pub permission: PermissionPolicy,
    /// 若提供 `termade` 執行檔路徑，adapter 會在啟動時將 `termade mcp` 注入該 agent。
    pub mcp_exe: Option<PathBuf>,
    /// 附加在系統提示 (Claude `--append-system-prompt`、Codex `developerInstructions`)
    /// 或 ACP 第一則 prompt 前面的協作說明；PTY 不注入。
    pub system_append: Option<String>,
    /// 啟動後立刻送出的第一個 prompt。
    pub initial_prompt: Option<String>,
    /// 初始 prompt 的附件。
    pub initial_attachments: Vec<termade_core::Attachment>,
    /// 接續既有對話：agent 原生的對話 id。`None` 表示開新對話。
    pub resume: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

/// 對一個執行中 session 的控制把手。
pub trait AgentControl: Send + Sync {
    /// 送一段使用者輸入 / prompt。
    fn send_input(&self, text: &str) -> Result<()>;
    /// 送 prompt 並附上檔案／圖片。預設把附件併進文字；支援圖片區塊的 adapter 會覆寫。
    fn send_input_with(&self, text: &str, attachments: &[termade_core::Attachment]) -> Result<()> {
        self.send_input(&termade_core::compose_text(text, attachments, true))
    }
    /// 中斷當前回合 (不殺處理程序)。
    fn cancel(&self) -> Result<()>;
    /// 結束並殺掉處理程序。
    fn kill(&self) -> Result<()>;
    /// 暫停子處理程序 (SIGSTOP，用於閒置休眠)。
    fn sleep(&self) -> Result<()> {
        Ok(())
    }
    /// 喚醒子處理程序 (SIGCONT)。
    fn wake(&self) -> Result<()> {
        Ok(())
    }
    /// 回覆核准請求。
    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()>;
    /// 這個 session 實際使用的傳輸方式 (顯示用)。
    fn transport(&self) -> &'static str;
    /// 取得 agent 主處理程序 PID。
    fn pid(&self) -> Option<u32> {
        None
    }
    /// 執行期切換 model 與／或 effort (下一個回合起生效)。
    /// 成功回傳實際生效的 model (agent 有回報才有)；不支援的 adapter 回傳明確錯誤。
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        let _ = (model, effort);
        bail!(
            "{}",
            tr!(
                "{transport} 不支援執行期切換 model／effort",
                transport = self.transport()
            )
        )
    }
}

/// 檢查 effort 值合法。
pub fn check_effort(e: &str) -> Result<()> {
    if !EFFORT_LEVELS.contains(&e) {
        bail!(
            "{}",
            tr!(
                "未知的 effort `{e}`；可用：{list}",
                e = e,
                list = EFFORT_LEVELS.join(", ")
            )
        );
    }
    Ok(())
}

/// 組出 `kill -s <NAME> -- <target>` 的參數。
///
/// 一律用 `-s NAME --` 形式：procps-ng 4.0.4 的 `kill -KILL -<pgid>` 會把 `-<pgid>`
/// 誤解析成 `-1` (送給使用者的所有處理程序)，等於 `kill -9 -1`，會把整個桌面登出。
fn kill_args(sig: &str, target: String) -> [String; 4] {
    let name = sig.trim_start_matches('-').trim_start_matches("SIG");
    ["-s".into(), name.into(), "--".into(), target]
}

fn run_kill(args: [String; 4]) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args(&args)
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| tr!("執行 kill 失敗"))?;
    if !status.success() {
        bail!("{}", tr!("kill {args} 失敗", args = args.join(" ")));
    }
    Ok(())
}

/// 對指定 PID 送出 Unix 訊號 (例如 `-STOP` / `-CONT`)。
pub fn signal_pid(pid: u32, sig: &str) -> Result<()> {
    // 0 / 1 會變成「自己的群組」或 init，絕不送。
    if pid <= 1 {
        bail!("{}", tr!("拒絕對 PID {pid} 送訊號", pid = pid));
    }
    run_kill(kill_args(sig, pid.to_string()))
}

/// 對指定處理程序群組 (PGID) 送出 Unix 訊號 (例如 `-STOP` / `-CONT` / `-KILL`)。
pub fn signal_pgid(pgid: u32, sig: &str) -> Result<()> {
    // `-0` 是自己的群組、`-1` 是所有處理程序，絕不送。
    if pgid <= 1 {
        bail!("{}", tr!("拒絕對 PID {pid} 送訊號", pid = pgid));
    }
    run_kill(kill_args(sig, format!("-{pgid}")))
}

#[cfg(test)]
mod signal_tests {
    use super::*;

    #[test]
    fn kill_args_never_put_negative_target_before_double_dash() {
        assert_eq!(
            kill_args("-KILL", "-4242".into()),
            ["-s", "KILL", "--", "-4242"]
        );
        assert_eq!(kill_args("-STOP", "42".into()), ["-s", "STOP", "--", "42"]);
        assert_eq!(
            kill_args("SIGCONT", "42".into()),
            ["-s", "CONT", "--", "42"]
        );
    }

    #[test]
    fn refuses_pid_zero_and_one() {
        assert!(signal_pgid(0, "-KILL").is_err());
        assert!(signal_pgid(1, "-KILL").is_err());
        assert!(signal_pid(0, "-KILL").is_err());
        assert!(signal_pid(1, "-KILL").is_err());
    }

    #[test]
    fn pgid_signal_hits_only_the_group() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        signal_pgid(child.id(), "-KILL").unwrap();
        let st = child.wait().unwrap();
        assert!(!st.success());
    }
}

/// 探測某個可執行程式是否存在，回傳絕對路徑。
pub fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(prog);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 找 agent 執行檔：先看環境變數覆寫 (測試與自訂安裝位置用)，否則在 PATH 找。
pub fn agent_bin(prog: &str, env_var: &str) -> Option<PathBuf> {
    match std::env::var(env_var) {
        Ok(p) if !p.is_empty() => which(&p),
        _ => which(prog),
    }
}

/// 推理強度的合法值。
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// 啟動一個 session，回傳控制把手；事件會從 `tx` 流出。
///
/// 依 `spec.transport` 決定用原生結構化協定還是 PTY 直通。
/// `Auto` 會挑該 agent 最適合的原生協定；若原生協定啟動失敗則退回 PTY。
pub fn spawn(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    // 不支援的細部權限選項要明確報錯，不能靜默忽略。
    let is_claude = matches!(spec.kind, AgentKind::ClaudeCode);
    if let Some(e) = &spec.effort {
        if !EFFORT_LEVELS.contains(&e.as_str()) {
            bail!(
                "{}",
                tr!(
                    "未知的 effort `{e}`；可用：{list}",
                    e = e,
                    list = EFFORT_LEVELS.join(", ")
                )
            );
        }
    }
    let is_codex = matches!(spec.kind, AgentKind::Codex);
    if !is_claude && (!spec.allowed_tools.is_empty() || !spec.disallowed_tools.is_empty()) {
        bail!(
            "{}",
            tr!(
                "{kind} 尚不支援 allowed_tools／disallowed_tools；請改用 claude",
                kind = spec.kind
            )
        );
    }
    if spec.read_only && !is_claude && !is_codex {
        bail!(
            "{}",
            tr!(
                "{kind} 尚不支援 read_only；請改用 claude 或 codex",
                kind = spec.kind
            )
        );
    }
    if spec.sandbox.is_some() && !is_codex {
        bail!("{}", tr!("sandbox 只適用於 codex"));
    }
    // 接續只走原生協定；不能靜默開新對話。
    if spec.resume.is_some() {
        if spec.transport == Transport::Pty {
            bail!("{}", tr!("PTY 模式無法接續對話"));
        }
        if matches!(spec.kind, AgentKind::Custom(_)) {
            bail!(
                "{}",
                tr!("{kind} 沒有原生結構化協定，無法接續對話", kind = spec.kind)
            );
        }
    }
    let want_structured = match spec.transport {
        _ if spec.resume.is_some() => true,
        Transport::Pty => false,
        Transport::Structured => true,
        Transport::Auto => supports_structured(&spec.kind),
    };

    if want_structured {
        match structured_start(spec.clone(), tx.clone()) {
            Ok(ctl) => return Ok(ctl),
            Err(e)
                if spec.transport == Transport::Auto
                    && spec.model.is_none()
                    && spec.resume.is_none() =>
            {
                tracing::warn!("{} 原生協定啟動失敗，退回 PTY：{e}", spec.kind);
            }
            Err(e) => return Err(e),
        }
    }

    if let Some(m) = &spec.model {
        if spec.transport != Transport::Pty {
            bail!(
                "{}",
                tr!(
                    "{kind} 尚不支援以結構化模式指定 model ({m})；請改用 --transport pty 並以參數傳入",
                    kind = spec.kind,
                    m = m
                )
            );
        }
    }
    let (prog, _) = spec.kind.pty_command();
    if which(prog).is_none() {
        bail!("{}", tr!("找不到 `{prog}`，請先安裝", prog = prog));
    }
    pty::start(spec, tx)
}

/// 這個 agent 是否有原生結構化協定可用。
pub fn supports_structured(kind: &AgentKind) -> bool {
    match kind {
        AgentKind::OpenCode | AgentKind::Antigravity => kind.acp_command().is_some(),
        AgentKind::ClaudeCode => agent_bin("claude", "TERMADE_CLAUDE_BIN").is_some(),
        AgentKind::Codex => which("codex").is_some(),
        AgentKind::Custom(_) => false,
    }
}

/// 檢查對應的結構化 agent 執行檔是否已安裝於本機 `$PATH`。
pub fn is_installed(kind: &AgentKind) -> bool {
    match kind {
        AgentKind::OpenCode | AgentKind::Antigravity => kind
            .acp_command()
            .is_some_and(|(prog, _)| which(prog).is_some()),
        AgentKind::ClaudeCode => agent_bin("claude", "TERMADE_CLAUDE_BIN").is_some(),
        AgentKind::Codex => which("codex").is_some(),
        AgentKind::Custom(_) => false,
    }
}

/// 依 agent 種類啟動對應的原生結構化 adapter。
fn structured_start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    match spec.kind {
        AgentKind::OpenCode | AgentKind::Antigravity => {
            let (prog, _) = spec
                .kind
                .acp_command()
                .with_context(|| tr!("此 agent 不支援 ACP"))?;
            let target = spec.bin.as_deref().unwrap_or(prog);
            if which(target).is_none() {
                bail!(
                    "{}",
                    tr!(
                        "找不到 `{target}`，請先安裝或改用 --transport pty",
                        target = target
                    )
                );
            }
            acp::start(spec, tx)
        }
        AgentKind::ClaudeCode => {
            let target = spec.bin.as_deref().unwrap_or("claude");
            if spec.bin.is_none() && std::env::var_os("TERMADE_CLAUDE_BIN").is_some() {
                // 由 claude::start 透過 TERMADE_CLAUDE_BIN 處理
            } else if which(target).is_none() {
                bail!(
                    "{}",
                    tr!(
                        "找不到 `{target}`，請先安裝或改用 --transport pty",
                        target = target
                    )
                );
            }
            claude::start(spec, tx)
        }
        AgentKind::Codex => {
            let target = spec.bin.as_deref().unwrap_or("codex");
            if which(target).is_none() {
                bail!(
                    "{}",
                    tr!(
                        "找不到 `{target}`，請先安裝或改用 --transport pty",
                        target = target
                    )
                );
            }
            codex::start(spec, tx)
        }
        AgentKind::Custom(_) => bail!(
            "{}",
            tr!("自訂 agent 沒有原生結構化協定，請用 --transport pty")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_sh_and_misses_nonsense() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }
}
