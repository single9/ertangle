//! PTY passthrough adapter.
//!
//! Treats the agent CLI as an interactive process running inside a pseudo-terminal: output directly becomes [`AgentEvent::Output`],
//! and user input is written back to the PTY. This approach **works for any CLI agent** and serves as a universal fallback.

use crate::{AgentControl, AgentEvent, SessionSpec};
use anyhow::{Context, Result};
use ertangle_core::{Decision, SessionStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tr::tr;

struct PtyControl {
    writer: Mutex<Box<dyn Write + Send>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
}

impl AgentControl for PtyControl {
    fn send_input(&self, text: &str) -> Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(text.as_bytes())?;
        w.write_all(b"\r")?;
        w.flush()?;
        Ok(())
    }

    fn cancel(&self) -> Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(&[0x03])?; // Ctrl-C
        w.flush()?;
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .process_id()
    }

    fn kill(&self) -> Result<()> {
        if let Some(pid) = self.pid() {
            // 強制終止處理程序群組或處理程序，若行程已結束則忽略
            let _ = crate::signal_pgid(pid, "-KILL").or_else(|_| crate::signal_pid(pid, "-KILL"));
        }
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .kill()
            .ok();
        Ok(())
    }

    fn sleep(&self) -> Result<()> {
        if let Some(pid) = self.pid() {
            crate::signal_pgid(pid, "-STOP").or_else(|_| crate::signal_pid(pid, "-STOP"))?;
        }
        Ok(())
    }

    fn wake(&self) -> Result<()> {
        if let Some(pid) = self.pid() {
            crate::signal_pgid(pid, "-CONT").or_else(|_| crate::signal_pid(pid, "-CONT"))?;
        }
        Ok(())
    }

    /// PTY has no structured protocol and cannot handle per-item permission approval.
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
        Ok(())
    }

    /// PTY resolves user questions by writing the answer string directly to stdin.
    fn resolve_question(&self, _id: &str, answers: &[String]) -> Result<()> {
        if let Some(ans) = answers.first() {
            self.send_input(&format!("{ans}\n"))?;
        }
        Ok(())
    }

    fn transport(&self) -> &'static str {
        "pty"
    }
}

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: spec.rows.max(24),
            cols: spec.cols.max(80),
            pixel_width: 0,
            pixel_height: 0,
        })
        .with_context(|| tr!("openpty 失敗"))?;

    let (default_prog, base) = spec.kind.pty_command();
    let prog = match spec.bin.as_deref() {
        Some(b) => crate::which(b).with_context(|| tr!("找不到指定的執行檔 `{b}`", b = b))?,
        None => crate::which(default_prog)
            .with_context(|| tr!("找不到 `{prog}`", prog = default_prog))?,
    };
    let mut cmd = CommandBuilder::new(prog);
    for a in base {
        cmd.arg(a);
    }
    for a in &spec.args {
        cmd.arg(a);
    }
    cmd.cwd(&spec.cwd);
    cmd.env("TERM", "xterm-256color");
    for k in crate::env_removals(&spec.kind) {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| tr!("spawn agent 失敗"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .with_context(|| tr!("clone reader 失敗"))?;
    let writer = pair
        .master
        .take_writer()
        .with_context(|| tr!("take writer 失敗"))?;
    drop(pair.master);

    // 接收端若已關閉則忽略狀態更新
    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));

    // Read output: strip terminal escape codes and send in real time, but skip redraw chunks that are entirely whitespace.
    //
    // Note: Fullscreen TUIs (opencode/claude/agy interactive mode) use screen-oriented output positioned
    // solely with escape sequences, so plain text extraction is inherently imperfect. For readable structured output,
    // use ACP / headless; this is a universal fallback. Full vt100 terminal emulation is deferred for later.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut stripper = crate::ansi::AnsiStripper::new();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = stripper.feed(buf.get(..n).unwrap_or_default());
                        if text.trim().is_empty() {
                            continue;
                        }
                        let trimmed = text.trim_end_matches(['\r', ' ', '\t']);
                        if tx.send(AgentEvent::Output(trimmed.to_string())).is_err() {
                            break;
                        }
                    }
                    Err(_) => break, // Linux returns EIO when the child process exits
                }
            }
        });
    }

    // Wait for termination (poll with try_wait to avoid holding the lock during a blocking wait, which would deadlock kill).
    let child = Arc::new(Mutex::new(child));
    {
        let child = child.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let code = loop {
                let status = {
                    let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
                    c.try_wait()
                };
                match status {
                    Ok(Some(s)) => break Some(s.exit_code() as i32),
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(_) => break None,
                }
            };
            // 接收端若已關閉則忽略退出事件
            let _ = tx.send(AgentEvent::Exited(code));
        });
    }

    let control = PtyControl {
        writer: Mutex::new(writer),
        child,
    };

    if let Some(p) = &spec.initial_prompt {
        // 寫入初始 prompt 失敗由後續事件與流程處理
        let _ = control.send_input_with(p, &spec.initial_attachments);
    }

    Ok(Box::new(control))
}
