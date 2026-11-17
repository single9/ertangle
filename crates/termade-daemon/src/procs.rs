//! 後代處理程序探測與過濾 (Linux / macOS)。

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::SystemTime;
use termade_protocol::BgProc;

/// 取得指定 root PID 的所有後代處理程序清單，並依規則過濾。
pub fn scan_session_procs(root_pid: u32, is_busy: bool) -> Vec<BgProc> {
    let pids = find_descendants(root_pid);
    let mut procs = Vec::new();

    for pid in pids {
        if let Some(proc) = inspect_proc(pid) {
            // 過濾 termade mcp
            if proc.cmdline.contains("termade mcp")
                || (proc.cmdline.contains("termade") && proc.cmdline.contains("mcp"))
            {
                continue;
            }
            // 若 session 處於 Busy，過濾前台同步執行中常見的 shell wrapper
            if is_busy {
                let is_shell = matches!(proc.name.as_str(), "sh" | "bash" | "zsh");
                let is_shell_exec = proc.cmdline.starts_with("sh -c")
                    || proc.cmdline.starts_with("bash -c")
                    || proc.cmdline.starts_with("/bin/sh -c")
                    || proc.cmdline.starts_with("/bin/bash -c");
                if is_shell && is_shell_exec {
                    continue;
                }
            }
            procs.push(proc);
        }
    }

    procs.sort_by_key(|p| p.pid);
    procs
}

/// 檢查 target_pid 是否為 root_pid 的後代處理程序。
pub fn is_descendant(root_pid: u32, target_pid: u32) -> bool {
    if root_pid == target_pid {
        return false;
    }
    find_descendants(root_pid).contains(&target_pid)
}

/// 找出 root_pid 的所有後代 PID (廣度優先走訪)。
pub fn find_descendants(root_pid: u32) -> Vec<u32> {
    let mut stack = vec![root_pid];
    let mut descendants = Vec::new();
    let mut visited = HashSet::new();
    visited.insert(root_pid);

    while let Some(curr) = stack.pop() {
        let children = get_children_pids(curr);
        for child in children {
            if visited.insert(child) {
                stack.push(child);
                descendants.push(child);
            }
        }
    }

    descendants
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
    days * 86400 + h * 3600 + m * 60 + sec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_etime_formats() {
        assert_eq!(parse_etime("05"), 5);
        assert_eq!(parse_etime("02:15"), 135);
        assert_eq!(parse_etime("01:02:03"), 3723);
        assert_eq!(parse_etime("2-01:02:03"), 2 * 86400 + 3723);
    }
}
