//! `ertangle dev`: non-blocking dev server manager (start / status / stop).
//!
//! The server runs in its own process group, logs to `.ertangle/dev-<port>.log`,
//! and its state lives in `.ertangle/dev-server.json` inside the workspace.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(clap::Subcommand, Debug, Clone)]
pub(crate) enum DevCmd {
    /// 在背景啟動開發伺服器 (自動分配空閒連接埠並等待就緒)
    Start {
        /// 目標工作目錄 (預設為目前目錄)
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// 指定連接埠 (省略時自動分配空閒連接埠)
        #[arg(long)]
        port: Option<u16>,
        /// 等待伺服器就緒的逾時秒數 (預設 25)
        #[arg(long, default_value = "25")]
        timeout_secs: u64,
        #[arg(long)]
        json: bool,
        /// 自訂啟動命令 (省略時依專案自動偵測 vite / dotnet / cargo)
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// 查看目前工作區受管開發伺服器的狀態
    Status {
        /// 目標工作目錄 (預設為目前目錄)
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// 停止目前工作區受管的開發伺服器
    Stop {
        /// 目標工作目錄 (預設為目前目錄)
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

/// Persisted state of a managed dev server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DevState {
    pub pid: u32,
    pub port: u16,
    pub url: String,
    pub cmd: Vec<String>,
    pub log_path: String,
    pub started_ms: u64,
}

impl DevState {
    fn to_json(&self) -> Value {
        json!({
            "pid": self.pid,
            "port": self.port,
            "url": self.url,
            "cmd": self.cmd,
            "log_path": self.log_path,
            "started_ms": self.started_ms,
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            port: u16::try_from(v.get("port")?.as_u64()?).ok()?,
            url: v.get("url")?.as_str()?.to_string(),
            cmd: v
                .get("cmd")?
                .as_array()?
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect(),
            log_path: v.get("log_path")?.as_str()?.to_string(),
            started_ms: v.get("started_ms")?.as_u64()?,
        })
    }
}

/// Ask the OS for a free loopback TCP port.
pub(crate) fn allocate_free_port() -> Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0").context("bind 127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

type CmdAndEnv = (Vec<String>, Vec<(String, String)>);

fn port_env(port: u16) -> Vec<(String, String)> {
    vec![
        ("PORT".into(), port.to_string()),
        ("VITE_PORT".into(), port.to_string()),
        ("ASPNETCORE_URLS".into(), format!("http://127.0.0.1:{port}")),
    ]
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Pick the dev command and environment for the project in `dir` (Node, ASP.NET Core, Cargo).
pub(crate) fn detect_dev_command(dir: &Path, port: u16) -> Result<CmdAndEnv> {
    if let Some(r) = detect_node(dir, port) {
        return Ok(r);
    }
    if let Some(r) = detect_dotnet(dir, port) {
        return Ok(r);
    }
    if dir.join("Cargo.toml").is_file() {
        return Ok((
            strs(&["cargo", "run"]),
            vec![("PORT".into(), port.to_string())],
        ));
    }
    bail!(
        "cannot detect dev command in {} (pass one after `--`)",
        dir.display()
    )
}

fn detect_node(dir: &Path, port: u16) -> Option<CmdAndEnv> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let pkg: Value = serde_json::from_str(&text).ok()?;
    let scripts = pkg.get("scripts")?;
    let script = ["dev", "start"]
        .into_iter()
        .find(|s| scripts.get(*s).is_some())?;
    let pm = if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("yarn.lock").exists() {
        "yarn"
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    };
    let cmd = vec![
        pm.to_string(),
        "run".into(),
        script.into(),
        "--".into(),
        "--port".into(),
        port.to_string(),
    ];
    let env = vec![
        ("PORT".into(), port.to_string()),
        ("VITE_PORT".into(), port.to_string()),
    ];
    Some((cmd, env))
}

fn find_web_csproj(dir: &Path) -> Option<(PathBuf, bool)> {
    let scan = |d: &Path| -> Option<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(d)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "csproj"))
            .filter(|p| fs::read_to_string(p).is_ok_and(|t| t.contains("Microsoft.NET.Sdk.Web")))
            .collect();
        found.sort();
        found.into_iter().next()
    };
    if let Some(p) = scan(dir) {
        return Some((p, true));
    }
    let mut subs: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    subs.sort();
    subs.iter().find_map(|s| scan(s).map(|p| (p, false)))
}

fn detect_dotnet(dir: &Path, port: u16) -> Option<CmdAndEnv> {
    let (csproj, at_root) = find_web_csproj(dir)?;
    let mut cmd = strs(&["dotnet", "run", "--no-launch-profile"]);
    if !at_root {
        cmd.push("--project".into());
        cmd.push(csproj.display().to_string());
    }
    let env = vec![
        ("ASPNETCORE_URLS".into(), format!("http://127.0.0.1:{port}")),
        ("DOTNET_NOLOGO".into(), "1".into()),
    ];
    Some((cmd, env))
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join(".ertangle").join("dev-server.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_state(dir: &Path) -> Option<DevState> {
    let text = fs::read_to_string(state_path(dir)).ok()?;
    DevState::from_json(&serde_json::from_str(&text).ok()?)
}

/// `kill -0` check that also treats zombies as dead.
fn is_alive(pid: u32) -> bool {
    let signalled = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !signalled {
        return false;
    }
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .and_then(|t| t.trim_start().chars().next())
            .is_none_or(|state| state != 'Z'),
        Err(_) => true,
    }
}

fn signal_group(pid: u32, sig: &str) {
    // `--` keeps `kill` from parsing `-<pgid>` as signal flags (T39).
    let _ = Command::new("kill")
        .args([sig, "--", &format!("-{pid}")])
        .stderr(Stdio::null())
        .status();
}

fn wait_dead(pid: u32, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !is_alive(pid)
}

fn log_tail(path: &Path, max_lines: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines.get(start..).unwrap_or(&[]).join("\n")
}

const READY_MARKERS: &[&str] = &[
    "http://localhost:",
    "http://127.0.0.1:",
    "Now listening on:",
];

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(100),
    )
    .is_ok()
}

fn log_ready(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|t| READY_MARKERS.iter().any(|m| t.contains(m)))
}

/// Stop the managed server (if any). Returns true if a live process was signalled.
pub(crate) fn stop_dev(dir: &Path) -> Result<bool> {
    let Some(st) = read_state(dir) else {
        return Ok(false);
    };
    let alive = is_alive(st.pid);
    if alive {
        signal_group(st.pid, "-TERM");
        if !wait_dead(st.pid, Duration::from_secs(3)) {
            signal_group(st.pid, "-KILL");
            wait_dead(st.pid, Duration::from_secs(2));
        }
    }
    // 移除狀態檔，若不存在則忽略
    let _ = fs::remove_file(state_path(dir));
    Ok(alive)
}

/// Current state and liveness, if a state file exists.
pub(crate) fn status_dev(dir: &Path) -> Option<(DevState, bool)> {
    let st = read_state(dir)?;
    let alive = is_alive(st.pid);
    Some((st, alive))
}

/// Start the server in the background and return once it is ready.
pub(crate) fn start_dev(
    dir: &Path,
    port: Option<u16>,
    timeout: Duration,
    cmd: Vec<String>,
) -> Result<DevState> {
    stop_dev(dir)?; // replace any previous managed server
    let port = match port {
        Some(p) => p,
        None => allocate_free_port()?,
    };
    let (argv, env) = if cmd.is_empty() {
        detect_dev_command(dir, port)?
    } else {
        (cmd, port_env(port))
    };
    let program = argv.first().ok_or_else(|| anyhow!("empty command"))?;
    let state_dir = dir.join(".ertangle");
    fs::create_dir_all(&state_dir).context("create .ertangle")?;
    let log_path = state_dir.join(format!("dev-{port}.log"));
    let log = File::create(&log_path).context("create dev log")?;
    let log_err = log.try_clone().context("clone dev log handle")?;

    let mut c = Command::new(program);
    c.args(argv.get(1..).unwrap_or(&[]))
        .current_dir(dir)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut child = c.spawn().with_context(|| format!("spawn {program}"))?;
    let pid = child.id();

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!(
                "dev server exited early (code {:?}):\n{}",
                status.code(),
                log_tail(&log_path, 40)
            );
        }
        if port_open(port) || log_ready(&log_path) {
            break;
        }
        if Instant::now() >= deadline {
            signal_group(pid, "-KILL");
            // 等待逾時被強制終止的子處理程序結束
            let _ = child.wait();
            bail!(
                "dev server not ready after {}s:\n{}",
                timeout.as_secs(),
                log_tail(&log_path, 40)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let st = DevState {
        pid,
        port,
        url: format!("http://127.0.0.1:{port}"),
        cmd: argv,
        log_path: log_path.display().to_string(),
        started_ms: now_ms(),
    };
    fs::write(
        state_path(dir),
        serde_json::to_string_pretty(&st.to_json())?,
    )
    .context("write dev-server.json")?;
    // 在背景執行緒回收子處理程序結束狀態，避免產生殭屍行程
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(st)
}

fn resolve_dir(cwd: Option<PathBuf>) -> Result<PathBuf> {
    match cwd {
        Some(d) => Ok(d),
        None => std::env::current_dir().context("current dir"),
    }
}

/// CLI entrypoint for `ertangle dev`.
pub(crate) fn cmd_dev(cmd: DevCmd) -> Result<()> {
    match cmd {
        DevCmd::Start {
            cwd,
            port,
            timeout_secs,
            json,
            cmd,
        } => {
            let dir = resolve_dir(cwd)?;
            let st = start_dev(&dir, port, Duration::from_secs(timeout_secs), cmd)?;
            if json {
                let mut v = st.to_json();
                if let Some(map) = v.as_object_mut() {
                    map.insert("ok".to_string(), json!(true));
                }
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!(
                    "dev server ready: {} (pid {}, log {})",
                    st.url, st.pid, st.log_path
                );
            }
        }
        DevCmd::Status { cwd, json } => {
            let dir = resolve_dir(cwd)?;
            match status_dev(&dir) {
                Some((st, alive)) => {
                    if json {
                        let mut v = st.to_json();
                        if let Some(map) = v.as_object_mut() {
                            map.insert("running".to_string(), json!(alive));
                        }
                        println!("{}", serde_json::to_string(&v)?);
                    } else if alive {
                        println!("running: {} (pid {}, log {})", st.url, st.pid, st.log_path);
                    } else {
                        println!("not running (stale state, last pid {})", st.pid);
                    }
                }
                None if json => println!("{}", json!({ "running": false })),
                None => println!("not running"),
            }
        }
        DevCmd::Stop { cwd, json } => {
            let dir = resolve_dir(cwd)?;
            let stopped = stop_dev(&dir)?;
            if json {
                println!("{}", json!({ "ok": true, "stopped": stopped }));
            } else if stopped {
                println!("dev server stopped");
            } else {
                println!("no running dev server");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        ertangle_test_support::temp_dir(&format!("ertangle-dev-{tag}"))
    }

    fn have_python() -> bool {
        Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn free_port_is_usable() {
        let p = allocate_free_port().unwrap();
        assert!(p > 0);
        assert!(TcpListener::bind(("127.0.0.1", p)).is_ok());
    }

    #[test]
    fn detects_node_with_package_manager() {
        let d = tmp("node");
        fs::write(d.join("package.json"), r#"{"scripts":{"dev":"vite"}}"#).unwrap();
        fs::write(d.join("pnpm-lock.yaml"), "").unwrap();
        let (cmd, env) = detect_dev_command(&d, 4321).unwrap();
        assert_eq!(cmd, strs(&["pnpm", "run", "dev", "--", "--port", "4321"]));
        assert!(env.contains(&("PORT".into(), "4321".into())));
        assert!(env.contains(&("VITE_PORT".into(), "4321".into())));
        fs::remove_file(d.join("pnpm-lock.yaml")).unwrap();
        let (cmd, _) = detect_dev_command(&d, 1).unwrap();
        assert_eq!(cmd[0], "npm");
    }

    #[test]
    fn detects_aspnet_root_and_subdir() {
        let d = tmp("dotnet");
        fs::create_dir_all(d.join("Api")).unwrap();
        fs::write(
            d.join("Api/Api.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web"></Project>"#,
        )
        .unwrap();
        let (cmd, env) = detect_dev_command(&d, 5055).unwrap();
        assert_eq!(
            &cmd[..3],
            &strs(&["dotnet", "run", "--no-launch-profile"])[..]
        );
        assert!(cmd.contains(&"--project".to_string()));
        assert!(env.contains(&("ASPNETCORE_URLS".into(), "http://127.0.0.1:5055".into())));
        assert!(env.contains(&("DOTNET_NOLOGO".into(), "1".into())));
        fs::write(
            d.join("Root.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web"></Project>"#,
        )
        .unwrap();
        let (cmd, _) = detect_dev_command(&d, 5055).unwrap();
        assert_eq!(cmd, strs(&["dotnet", "run", "--no-launch-profile"]));
    }

    #[test]
    fn detects_cargo_and_errors_when_unknown() {
        let d = tmp("cargo");
        assert!(detect_dev_command(&d, 1).is_err());
        fs::write(d.join("Cargo.toml"), "[package]\n").unwrap();
        let (cmd, env) = detect_dev_command(&d, 7000).unwrap();
        assert_eq!(cmd, strs(&["cargo", "run"]));
        assert_eq!(env, vec![("PORT".to_string(), "7000".to_string())]);
    }

    #[test]
    fn state_json_roundtrip() {
        let st = DevState {
            pid: 42,
            port: 8080,
            url: "http://127.0.0.1:8080".into(),
            cmd: strs(&["a", "b"]),
            log_path: "x.log".into(),
            started_ms: 7,
        };
        assert_eq!(DevState::from_json(&st.to_json()), Some(st));
        assert_eq!(DevState::from_json(&json!({"pid": 1})), None);
    }

    #[test]
    fn lifecycle_start_status_stop() {
        if !have_python() {
            return;
        }
        let d = tmp("life");
        let port = allocate_free_port().unwrap();
        let cmd = vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            port.to_string(),
            "--bind".into(),
            "127.0.0.1".into(),
        ];
        let st = start_dev(&d, Some(port), Duration::from_secs(20), cmd).unwrap();
        assert_eq!(st.port, port);
        assert!(port_open(port));
        let (seen, alive) = status_dev(&d).unwrap();
        assert!(alive);
        assert_eq!(seen.pid, st.pid);
        assert!(stop_dev(&d).unwrap());
        assert!(!is_alive(st.pid));
        assert!(status_dev(&d).is_none());
        assert!(!stop_dev(&d).unwrap());
    }

    #[test]
    fn early_exit_reports_log_tail() {
        let d = tmp("early");
        let cmd = strs(&["sh", "-c", "echo boom-output; exit 3"]);
        let err = start_dev(&d, None, Duration::from_secs(10), cmd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("exited early"), "{err}");
        assert!(err.contains("Some(3)"), "{err}");
        assert!(err.contains("boom-output"), "{err}");
        assert!(status_dev(&d).is_none());
    }

    #[test]
    fn timeout_kills_unready_server() {
        let d = tmp("timeout");
        let cmd = strs(&["sleep", "30"]);
        let err = start_dev(&d, None, Duration::from_millis(400), cmd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not ready"), "{err}");
    }
}
