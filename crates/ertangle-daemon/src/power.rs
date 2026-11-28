//! OS 層級的防休眠 (keep-awake)：daemon 執行期間阻止系統閒置休眠。
//!
//! 預設關閉，需在全域設定 `[power] keep_awake = true` 或以 `ertangle daemon --keep-awake` 啟用。
//! 實作上不呼叫任何 FFI，而是啟動平台內建的 inhibitor 子處理程序並持有到 [`WakeLock`] 被釋放：
//!
//! - macOS：`caffeinate -d -i -m -s -w <pid>` (daemon 意外結束時 caffeinate 也會自行退出)。
//! - Linux：`systemd-inhibit ... tail --pid=<pid> -f /dev/null`；用 `tail --pid` 而非
//!   `sleep infinity`，daemon 被 SIGKILL 時被抑制的命令也會跟著結束，不會留下孤兒。
//! - 其他平台：不支援，記一筆警告後以未鎖定狀態繼續。

use std::process::{Child, Command, Stdio};

/// 目前執行的作業系統類型 (可注入，方便測試各平台分支)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Other,
}

impl Platform {
    /// 編譯目標的平台。
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }
}

/// 要啟動的 inhibitor 命令 (程式名稱與參數)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InhibitCommand {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// 依平台決定 inhibitor 命令；不支援的平台回 `None`。
pub fn inhibit_command(platform: Platform, pid: u32) -> Option<InhibitCommand> {
    match platform {
        Platform::MacOs => Some(InhibitCommand {
            program: "caffeinate",
            args: ["-d", "-i", "-m", "-s", "-w"]
                .iter()
                .map(|s| s.to_string())
                .chain(std::iter::once(pid.to_string()))
                .collect(),
        }),
        Platform::Linux => Some(InhibitCommand {
            program: "systemd-inhibit",
            args: vec![
                "--what=idle:sleep".to_string(),
                "--who=ertangle".to_string(),
                "--why=ertangle daemon active".to_string(),
                "--mode=block".to_string(),
                "tail".to_string(),
                format!("--pid={pid}"),
                "-f".to_string(),
                "/dev/null".to_string(),
            ],
        }),
        Platform::Other => None,
    }
}

/// 一個已啟動、需在釋放時收掉的 inhibitor。
pub trait Inhibitor: Send {
    /// 釋放 inhibitor；必須可重複呼叫且絕不 panic。
    fn release(&mut self);
}

/// 啟動 inhibitor 的抽象 (正式環境用 [`SystemLauncher`]，測試可注入假的)。
pub trait Launcher {
    fn launch(&self, cmd: &InhibitCommand) -> std::io::Result<Box<dyn Inhibitor>>;
}

/// 以子處理程序實作的 inhibitor。
struct ChildInhibitor(Option<Child>);

impl Inhibitor for ChildInhibitor {
    fn release(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        // 子處理程序可能已自行結束；kill 失敗不影響後續，wait 只為回收避免殭屍。
        if let Err(e) = child.kill() {
            tracing::debug!("結束 keep-awake 子處理程序失敗：{e}");
        }
        if let Err(e) = child.wait() {
            tracing::debug!("回收 keep-awake 子處理程序失敗：{e}");
        }
    }
}

/// 真正啟動系統命令的 launcher。
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn launch(&self, cmd: &InhibitCommand) -> std::io::Result<Box<dyn Inhibitor>> {
        let child = Command::new(cmd.program)
            .args(&cmd.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Box::new(ChildInhibitor(Some(child))))
    }
}

/// RAII 防休眠鎖：持有期間阻止系統閒置休眠，drop (或 [`WakeLock::release`]) 時解除。
pub struct WakeLock {
    inner: Option<Box<dyn Inhibitor>>,
}

impl std::fmt::Debug for WakeLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeLock")
            .field("active", &self.is_active())
            .finish()
    }
}

impl WakeLock {
    /// 未鎖定的 no-op 鎖。
    pub fn disabled() -> Self {
        WakeLock { inner: None }
    }

    /// 以目前平台與 daemon 自身 PID 取得鎖；失敗只記警告，回傳未鎖定的鎖。
    pub fn acquire(enabled: bool) -> Self {
        Self::acquire_with(
            enabled,
            Platform::current(),
            std::process::id(),
            &SystemLauncher,
        )
    }

    /// 可注入平台與 launcher 的版本 (供測試)。
    pub fn acquire_with(
        enabled: bool,
        platform: Platform,
        pid: u32,
        launcher: &dyn Launcher,
    ) -> Self {
        if !enabled {
            return Self::disabled();
        }
        let Some(cmd) = inhibit_command(platform, pid) else {
            tracing::warn!("keep_awake 已啟用，但此平台不支援防休眠，將以未鎖定狀態繼續");
            return Self::disabled();
        };
        match launcher.launch(&cmd) {
            Ok(inhibitor) => {
                tracing::info!("已啟用防休眠 ({})", cmd.program);
                WakeLock {
                    inner: Some(inhibitor),
                }
            }
            Err(e) => {
                tracing::warn!(
                    "keep_awake 已啟用，但無法啟動 {} ({e})，將以未鎖定狀態繼續",
                    cmd.program
                );
                Self::disabled()
            }
        }
    }

    /// 是否真的持有 inhibitor。
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    /// 提前解除鎖 (例如 `process::exit` 前，Drop 不會被執行)。可重複呼叫。
    pub fn release(&mut self) {
        if let Some(mut inhibitor) = self.inner.take() {
            inhibitor.release();
        }
    }
}

impl Drop for WakeLock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeLauncher {
        fail: bool,
        launched: Mutex<Vec<InhibitCommand>>,
        released: Arc<AtomicUsize>,
    }

    struct FakeInhibitor(Arc<AtomicUsize>);

    impl Inhibitor for FakeInhibitor {
        fn release(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, cmd: &InhibitCommand) -> std::io::Result<Box<dyn Inhibitor>> {
            self.launched
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(cmd.clone());
            if self.fail {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
            }
            Ok(Box::new(FakeInhibitor(self.released.clone())))
        }
    }

    impl FakeLauncher {
        fn launched(&self) -> Vec<InhibitCommand> {
            self.launched
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    #[test]
    fn macos_uses_caffeinate_bound_to_pid() {
        let cmd = inhibit_command(Platform::MacOs, 42);
        assert_eq!(
            cmd,
            Some(InhibitCommand {
                program: "caffeinate",
                args: ["-d", "-i", "-m", "-s", "-w", "42"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            })
        );
    }

    #[test]
    fn linux_uses_systemd_inhibit_bound_to_pid() {
        let cmd = inhibit_command(Platform::Linux, 7);
        assert_eq!(cmd.as_ref().map(|c| c.program), Some("systemd-inhibit"));
        let args = cmd.map(|c| c.args).unwrap_or_default();
        assert!(args.contains(&"--what=idle:sleep".to_string()));
        assert!(args.contains(&"--mode=block".to_string()));
        assert!(args.contains(&"--pid=7".to_string()));
    }

    #[test]
    fn other_platform_is_unsupported() {
        assert_eq!(inhibit_command(Platform::Other, 1), None);
        let launcher = FakeLauncher::default();
        let lock = WakeLock::acquire_with(true, Platform::Other, 1, &launcher);
        assert!(!lock.is_active());
        assert!(launcher.launched().is_empty());
    }

    #[test]
    fn disabled_never_launches() {
        let launcher = FakeLauncher::default();
        let lock = WakeLock::acquire_with(false, Platform::Linux, 1, &launcher);
        assert!(!lock.is_active());
        assert!(launcher.launched().is_empty());
    }

    #[test]
    fn launch_failure_continues_unlocked() {
        let launcher = FakeLauncher {
            fail: true,
            ..Default::default()
        };
        let lock = WakeLock::acquire_with(true, Platform::Linux, 1, &launcher);
        assert!(!lock.is_active());
        assert_eq!(launcher.launched().len(), 1);
    }

    #[test]
    fn drop_releases_exactly_once() {
        let launcher = FakeLauncher::default();
        let lock = WakeLock::acquire_with(true, Platform::MacOs, 1, &launcher);
        assert!(lock.is_active());
        drop(lock);
        assert_eq!(launcher.released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn explicit_release_is_idempotent() {
        let launcher = FakeLauncher::default();
        let mut lock = WakeLock::acquire_with(true, Platform::Linux, 1, &launcher);
        lock.release();
        lock.release();
        assert!(!lock.is_active());
        drop(lock);
        assert_eq!(launcher.released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn child_inhibitor_kills_real_process() {
        let Ok(child) = Command::new("sleep").arg("30").spawn() else {
            return; // 沒有 sleep 的環境略過
        };
        let mut inh = ChildInhibitor(Some(child));
        inh.release();
        inh.release();
        assert!(inh.0.is_none());
    }
}
