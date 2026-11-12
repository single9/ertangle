//! PTY 直通 adapter。
//!
//! 把 agent CLI 當成一個在偽終端裡跑的互動程序：輸出直接變成 [`AgentEvent::Output`]，
//! 使用者輸入寫回 PTY。這條路**對任何 CLI agent 都有效**，是萬用後備。

use crate::{AgentControl, AgentEvent, SessionSpec};
use ade_core::{Decision, SessionStatus};
use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

struct PtyControl {
    writer: Mutex<Box<dyn Write + Send>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
}

impl AgentControl for PtyControl {
    fn send_input(&self, text: &str) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(text.as_bytes())?;
        w.write_all(b"\r")?;
        w.flush()?;
        Ok(())
    }

    fn cancel(&self) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(&[0x03])?; // Ctrl-C
        w.flush()?;
        Ok(())
    }

    fn kill(&self) -> Result<()> {
        self.child.lock().unwrap().kill().ok();
        Ok(())
    }

    fn sleep(&self) -> Result<()> {
        if let Some(pid) = self.child.lock().unwrap().process_id() {
            crate::signal_pid(pid, "-STOP")?;
        }
        Ok(())
    }

    fn wake(&self) -> Result<()> {
        if let Some(pid) = self.child.lock().unwrap().process_id() {
            crate::signal_pid(pid, "-CONT")?;
        }
        Ok(())
    }

    /// PTY 沒有結構化協定，無法逐項審批。
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
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
        .context("openpty 失敗")?;

    let (prog, base) = spec.kind.pty_command();
    let mut cmd = CommandBuilder::new(prog);
    for a in base {
        cmd.arg(a);
    }
    for a in &spec.args {
        cmd.arg(a);
    }
    cmd.cwd(&spec.cwd);
    cmd.env("TERM", "xterm-256color");
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }

    let child = pair.slave.spawn_command(cmd).context("spawn agent 失敗")?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .context("clone reader 失敗")?;
    let writer = pair.master.take_writer().context("take writer 失敗")?;
    drop(pair.master);

    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));

    // 讀取輸出：剝掉終端控制碼後即時送出，但略過「整段都是空白」的重繪 chunk。
    //
    // 註：全螢幕 TUI（opencode/claude/agy 互動模式）是螢幕導向輸出，只靠 escape
    // 序列定位，純文字抽取天生不完美。可讀的結構化輸出請走 ACP / headless；
    // 這裡是萬用後備。完整的 vt100 終端模擬留待後續。
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut stripper = crate::ansi::AnsiStripper::new();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = stripper.feed(&buf[..n]);
                        if text.trim().is_empty() {
                            continue;
                        }
                        let trimmed = text.trim_end_matches(['\r', ' ', '\t']);
                        if tx.send(AgentEvent::Output(trimmed.to_string())).is_err() {
                            break;
                        }
                    }
                    Err(_) => break, // 子程序結束時 Linux 會回 EIO
                }
            }
        });
    }

    // 等待結束（用 try_wait 輪詢，避免在阻塞 wait 期間一直握著鎖而讓 kill 死鎖）。
    let child = Arc::new(Mutex::new(child));
    {
        let child = child.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let code = loop {
                let status = {
                    let mut c = child.lock().unwrap();
                    c.try_wait()
                };
                match status {
                    Ok(Some(s)) => break Some(s.exit_code() as i32),
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(_) => break None,
                }
            };
            let _ = tx.send(AgentEvent::Exited(code));
        });
    }

    let control = PtyControl {
        writer: Mutex::new(writer),
        child,
    };

    if let Some(p) = &spec.initial_prompt {
        let _ = control.send_input(p);
    }

    Ok(Box::new(control))
}
