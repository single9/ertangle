//! 後代處理程序探測與過濾 (Linux / macOS)。

use ertangle_protocol::BgProc;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// 一個處理程序在後代樹中的節點：PID、父 PID、名稱 (comm) 與命令列。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcNode {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub cmdline: String,
}

/// 是否為「shell 執行一段指令」的 wrapper (`sh -c …`、`/usr/bin/zsh -c …`、`bash -lc …`)。
/// agent 的工具 (Claude 的 Bash、Codex 的 exec…) 都透過這種 wrapper 執行指令。
pub fn is_shell_wrapper(name: &str, cmdline: &str) -> bool {
    let mut argv = cmdline.split_whitespace();
    let exe = argv.next().unwrap_or("");
    let base = Path::new(exe)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let is_shell = |s: &str| matches!(s, "sh" | "bash" | "zsh" | "dash" | "fish" | "ksh");
    if !(is_shell(name) || is_shell(&base)) {
        return false;
    }
    argv.take_while(|a| a.starts_with('-'))
        .any(|a| !a.starts_with("--") && a.contains('c'))
}

/// agent 工具指令的子樹：agent 主處理程序的直接子處理程序中，屬於 shell wrapper 的那些及其所有後代。
/// MCP 伺服器、LSP 等 agent 自己帶起來的基礎設施 (非 shell 的直接子處理程序) 整棵排除，
/// 避免把使用者設定的其他程式列成背景處理程序，甚至被誤殺。
pub fn tool_subtree(root_pid: u32, nodes: &[ProcNode]) -> Vec<&ProcNode> {
    let mut keep: HashSet<u32> = nodes
        .iter()
        .filter(|n| n.ppid == root_pid && is_shell_wrapper(&n.name, &n.cmdline))
        .map(|n| n.pid)
        .collect();
    // nodes 依走訪順序排列 (父節點必在子節點之前)，一次掃描即可擴展整棵子樹。
    let mut out = Vec::new();
    for n in nodes {
        if keep.contains(&n.pid) || keep.contains(&n.ppid) {
            keep.insert(n.pid);
            out.push(n);
        }
    }
    out
}

/// 要列給使用者看的處理程序：在工具子樹中穿過 shell wrapper，每條分支只取第一個「實際指令」，
/// 不再列出它底下的 helper (例如瀏覽器的眾多子處理程序)。沒有子處理程序的 wrapper 在 session
/// 閒置時仍列出 (可能是常駐的 shell 迴圈)；忙碌時則視為前景工具呼叫而略過。
pub fn visible_tool_procs(root_pid: u32, nodes: &[ProcNode], is_busy: bool) -> Vec<u32> {
    let subtree = tool_subtree(root_pid, nodes);
    let pids: HashSet<u32> = subtree.iter().map(|n| n.pid).collect();
    let is_wrapper: HashSet<u32> = subtree
        .iter()
        .filter(|n| is_shell_wrapper(&n.name, &n.cmdline))
        .map(|n| n.pid)
        .collect();
    let has_child: HashSet<u32> = subtree.iter().map(|n| n.ppid).collect();
    subtree
        .iter()
        .filter(|n| {
            // 父節點必須是 root 或 wrapper：已列出的實際指令，其子孫不再重複列出。
            let parent_ok =
                n.ppid == root_pid || (pids.contains(&n.ppid) && is_wrapper.contains(&n.ppid));
            if !parent_ok {
                return false;
            }
            if is_wrapper.contains(&n.pid) {
                return !is_busy && !has_child.contains(&n.pid);
            }
            true
        })
        .map(|n| n.pid)
        .collect()
}

/// 取得指定 root PID 的背景處理程序清單 (只含 agent 工具指令啟動的處理程序)。
pub fn scan_session_procs(root_pid: u32, is_busy: bool) -> Vec<BgProc> {
    let nodes = collect_tree(root_pid);
    let mut procs: Vec<BgProc> = visible_tool_procs(root_pid, &nodes, is_busy)
        .into_iter()
        .filter_map(inspect_proc)
        .collect();
    procs.sort_by_key(|p| p.pid);
    procs
}

/// 可以由使用者終止的 PID：只限 agent 工具指令的子樹，不含 MCP 伺服器等基礎設施。
pub fn killable_pids(root_pid: u32) -> Vec<u32> {
    let nodes = collect_tree(root_pid);
    tool_subtree(root_pid, &nodes)
        .into_iter()
        .map(|n| n.pid)
        .collect()
}

/// 檢查 target_pid 是否為 root_pid 工具指令子樹中的處理程序 (可安全終止)。
pub fn is_killable(root_pid: u32, target_pid: u32) -> bool {
    target_pid != root_pid && killable_pids(root_pid).contains(&target_pid)
}

/// 走訪 root_pid 的後代樹，回傳每個節點 (父節點一定排在子節點之前)。
pub fn collect_tree(root_pid: u32) -> Vec<ProcNode> {
    let mut queue = std::collections::VecDeque::from([root_pid]);
    let mut nodes = Vec::new();
    let mut visited = HashSet::new();
    visited.insert(root_pid);

    while let Some(curr) = queue.pop_front() {
        for child in get_children_pids(curr) {
            if visited.insert(child) {
                queue.push_back(child);
                let (name, cmdline) = proc_name_and_cmdline(child);
                nodes.push(ProcNode {
                    pid: child,
                    ppid: curr,
                    name,
                    cmdline,
                });
            }
        }
    }

    nodes
}

/// 讀取處理程序名稱與命令列 (Linux 讀 /proc，其他平台呼叫 ps)。
fn proc_name_and_cmdline(pid: u32) -> (String, String) {
    match inspect_proc(pid) {
        Some(p) => (p.name, p.cmdline),
        None => (String::new(), String::new()),
    }
}

/// 取得單一處理程序的直接子處理程序清單。
fn get_children_pids(pid: u32) -> Vec<u32> {
    // 優先在 Linux 讀取 /proc/<pid>/task/<tid>/children
    let task_dir = format!("/proc/{pid}/task");
    if Path::new(&task_dir).is_dir() {
        let mut children = Vec::new();
        if let Ok(entries) = fs::read_dir(&task_dir) {
            for entry in entries.flatten() {
                let children_file = entry.path().join("children");
                if let Ok(content) = fs::read_to_string(&children_file) {
                    for part in content.split_whitespace() {
                        if let Ok(cpid) = part.parse::<u32>() {
                            children.push(cpid);
                        }
                    }
                }
            }
        }
        if !children.is_empty() {
            return children;
        }
    }

    // 後備支援 (macOS 或未掛載 procfs 的環境)：呼叫 pgrep -P
    if let Ok(output) = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            return s
                .lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .collect();
        }
    }

    Vec::new()
}

/// 讀取單一處理程序的名稱、命令列與執行時間。
fn inspect_proc(pid: u32) -> Option<BgProc> {
    let proc_path = format!("/proc/{pid}");
    if Path::new(&proc_path).is_dir() {
        // Linux /proc
        let cmdline = fs::read(format!("{proc_path}/cmdline"))
            .ok()
            .map(|bytes| {
                bytes
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        let comm = fs::read_to_string(format!("{proc_path}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let name = comm.unwrap_or_else(|| {
            cmdline
                .split_whitespace()
                .next()
                .map(|p| {
                    Path::new(p)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .unwrap_or_default()
        });

        let elapsed_secs = fs::metadata(&proc_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if !name.is_empty() || !cmdline.is_empty() {
            return Some(BgProc {
                pid,
                name: if name.is_empty() {
                    pid.to_string()
                } else {
                    name
                },
                cmdline,
                elapsed_secs,
            });
        }
    }

    // 後備支援 (macOS)：呼叫 ps -o pid=,etime=,comm=,args= -p <pid>
    if let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "pid=,etime=,comm=,args=", "-p", &pid.to_string()])
        .output()
    {
        if output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            let line = out.lines().next()?.trim();
            let mut parts = line.split_whitespace();
            let _ps_pid = parts.next()?;
            let etime_str = parts.next()?;
            let comm = parts.next().unwrap_or("").to_string();
            let args: Vec<&str> = parts.collect();
            let cmdline = args.join(" ");
            let elapsed_secs = parse_etime(etime_str);
            let name = if !comm.is_empty() {
                Path::new(&comm)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            } else {
                pid.to_string()
            };
            return Some(BgProc {
                pid,
                name,
                cmdline: if cmdline.is_empty() {
                    name_clone(&comm)
                } else {
                    cmdline
                },
                elapsed_secs,
            });
        }
    }

    None
}

fn name_clone(s: &str) -> String {
    s.to_string()
}

/// 解析 ps 的 etime 格式：`[[dd-]hh:]mm:ss`
pub fn parse_etime(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    let (days, time_part) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().unwrap_or(0), rest),
        None => (0, s),
    };
    let parts: Vec<&str> = time_part.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [sec] => (0, 0, sec.parse::<u64>().unwrap_or(0)),
        [m, sec] => (
            0,
            m.parse::<u64>().unwrap_or(0),
            sec.parse::<u64>().unwrap_or(0),
        ),
        [h, m, sec] => (
            h.parse::<u64>().unwrap_or(0),
            m.parse::<u64>().unwrap_or(0),
            sec.parse::<u64>().unwrap_or(0),
        ),
        _ => (0, 0, 0),
    };
    days.saturating_mul(86400)
        .saturating_add(h.saturating_mul(3600))
        .saturating_add(m.saturating_mul(60))
        .saturating_add(sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(pid: u32, ppid: u32, name: &str, cmdline: &str) -> ProcNode {
        ProcNode {
            pid,
            ppid,
            name: name.into(),
            cmdline: cmdline.into(),
        }
    }

    #[test]
    fn shell_wrapper_detection() {
        assert!(is_shell_wrapper(
            "zsh",
            "/usr/bin/zsh -c source snap.sh && eval 'ls'"
        ));
        assert!(is_shell_wrapper("bash", "bash -lc npm run dev"));
        assert!(is_shell_wrapper("bash", "/bin/bash --login -c make"));
        assert!(!is_shell_wrapper("bash", "bash"));
        assert!(!is_shell_wrapper("bash", "bash script.sh"));
        assert!(!is_shell_wrapper("node", "node -c x"));
    }

    #[test]
    fn visible_procs_skip_mcp_servers_wrappers_and_helpers() {
        let root = 100;
        let nodes = vec![
            // agent 帶起的 MCP 伺服器與它的子處理程序：整棵排除
            node(101, root, "siacheh", "/opt/siacheh mcp --project x"),
            node(
                102,
                root,
                "ertangle",
                "/home/u/.local/bin/ertangle mcp --session s",
            ),
            node(110, 101, "sh", "sh -c helper"),
            // 工具指令：wrapper → node → 瀏覽器 helper
            node(
                103,
                root,
                "zsh",
                "/usr/bin/zsh -c source snap && eval 'node probe.js'",
            ),
            node(104, 103, "node", "node probe.js"),
            node(105, 104, "chrome", "chrome --headless"),
            node(106, 105, "chrome", "chrome --type=renderer"),
            // 背景工作：巢狀 wrapper 後才是實際指令
            node(107, root, "bash", "bash -c (npm run dev &)"),
            node(108, 107, "sh", "sh -c vite"),
            node(109, 108, "vite", "node vite"),
            // 沒有子處理程序的 wrapper (常駐 shell 迴圈)
            node(111, root, "sh", "sh -c while true; do :; done"),
        ];
        assert_eq!(visible_tool_procs(root, &nodes, false), vec![104, 109, 111]);
        // 忙碌時無子處理程序的 wrapper 視為前景工具呼叫
        assert_eq!(visible_tool_procs(root, &nodes, true), vec![104, 109]);

        let killable: Vec<u32> = tool_subtree(root, &nodes).iter().map(|n| n.pid).collect();
        assert_eq!(killable, vec![103, 104, 105, 106, 107, 108, 109, 111]);
    }

    #[test]
    fn parse_etime_formats() {
        assert_eq!(parse_etime("05"), 5);
        assert_eq!(parse_etime("02:15"), 135);
        assert_eq!(parse_etime("01:02:03"), 3723);
        assert_eq!(parse_etime("2-01:02:03"), 2 * 86400 + 3723);
    }
}
