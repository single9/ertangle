use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProjectStack {
    Rust,
    Node,
    DotNet,
    Python,
    Godot,
}

impl ProjectStack {
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Node => "node",
            Self::DotNet => "dotnet",
            Self::Python => "python",
            Self::Godot => "godot",
        }
    }
}

pub(crate) fn detect_stacks(dir: &Path) -> Vec<ProjectStack> {
    let mut stacks = Vec::new();
    if dir.join("Cargo.toml").exists() {
        stacks.push(ProjectStack::Rust);
    }
    if dir.join("package.json").exists() {
        stacks.push(ProjectStack::Node);
    }
    if has_dotnet_project(dir) {
        stacks.push(ProjectStack::DotNet);
    }
    if ["pyproject.toml", "pytest.ini", "setup.py"]
        .iter()
        .any(|name| dir.join(name).exists())
    {
        stacks.push(ProjectStack::Python);
    }
    if dir.join("project.godot").is_file() {
        stacks.push(ProjectStack::Godot);
    }
    stacks
}

fn has_dotnet_project(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|v| v.to_str()),
                Some("sln" | "slnx" | "csproj")
            )
        {
            return true;
        }
        if path.is_dir() {
            let Ok(children) = fs::read_dir(path) else {
                continue;
            };
            if children
                .flatten()
                .any(|child| child.path().extension().and_then(|v| v.to_str()) == Some("csproj"))
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn strip_ansi(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(&ch) = chars.get(i) else { break };
        if ch == '\x1b' && chars.get(i + 1) == Some(&'[') {
            i += 2;
            while let Some(&c) = chars.get(i) {
                i += 1;
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if ch == '\x1b' && chars.get(i + 1) == Some(&']') {
            i += 2;
            while let Some(&c) = chars.get(i) {
                if c == '\x07' {
                    i += 1;
                    break;
                }
                if c == '\x1b' && chars.get(i + 1) == Some(&'\\') {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else {
            result.push(ch);
            i += 1;
        }
    }
    result
}

pub(crate) fn dehydrate_cargo_json(jsonl: &str, workspace_root: &Path) -> (usize, Vec<String>) {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for raw in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if value.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(level) = message.get("level").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(level, "error" | "warning") {
            continue;
        }
        let text = message.get("message").and_then(Value::as_str).unwrap_or("");
        let code = message.pointer("/code/code").and_then(Value::as_str);
        let span = message
            .get("spans")
            .and_then(Value::as_array)
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
                    .or_else(|| spans.first())
            });
        let location = span
            .map(|span| {
                let file = span.get("file_name").and_then(Value::as_str).unwrap_or("?");
                let file = normalize_path(file, workspace_root);
                let line = span.get("line_start").and_then(Value::as_u64).unwrap_or(0);
                let column = span
                    .get("column_start")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                format!("{file}:{line}:{column}")
            })
            .unwrap_or_else(|| "?:0:0".to_owned());
        let code_part = code.map(|code| format!("[{code}]")).unwrap_or_default();
        let line = format!("{location}: {level}{code_part}: {text}");
        if seen.insert(line.clone()) {
            lines.push(line);
        }
    }
    (lines.len(), lines)
}

pub(crate) fn dehydrate_cargo_test_output(output: &str) -> (usize, usize, Vec<String>) {
    let mut passed = 0;
    let mut failed = 0;
    let mut keep = Vec::new();
    let mut in_failure = false;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("test result:") {
            let fields: Vec<_> = trimmed.split_whitespace().collect();
            for pair in fields.windows(2) {
                if let (Some(p0), Some(p1)) = (pair.first(), pair.get(1)) {
                    if let Ok(count) = p0.parse::<usize>() {
                        match p1.trim_end_matches(';') {
                            "passed" => passed += count,
                            "failed" => failed += count,
                            _ => {}
                        }
                    }
                }
            }
            continue;
        }
        if trimmed.starts_with("---- ") && trimmed.ends_with(" stdout ----") {
            in_failure = true;
        }
        if trimmed == "failures:" || trimmed.starts_with("failures:") {
            in_failure = true;
        }
        if trimmed.starts_with("Compiling ")
            || trimmed.starts_with("Finished ")
            || trimmed.starts_with("Running unittests ")
        {
            continue;
        }
        if trimmed.starts_with("test ") && trimmed.ends_with(" ... ok") {
            continue;
        }
        if in_failure && !trimmed.is_empty() {
            keep.push(trimmed.to_owned());
        }
    }
    (passed, failed, keep)
}

pub(crate) fn dehydrate_node_output(raw: &str, workspace_root: &Path) -> Vec<String> {
    strip_ansi(raw)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty()
                || is_node_noise(line)
                || (line.starts_with("at ") && line.contains("node_modules/"))
            {
                return None;
            }
            Some(normalize_path(line, workspace_root))
        })
        .collect()
}

fn is_node_noise(line: &str) -> bool {
    [
        "[vite] ", "vite v", "webpack ", "[webpack", "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇",
        "⠏",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

pub(crate) fn dehydrate_dotnet_output(raw: &str, workspace_root: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for line in strip_ansi(raw).lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || is_dotnet_noise(trimmed)
            || trimmed.contains("at System.Runtime.CompilerServices.TaskAwaiter")
            || trimmed.starts_with("--- End of stack trace from previous location ---")
        {
            continue;
        }
        let mut line = trimmed.to_owned();
        if let Some(start) = line.rfind(" [") {
            if line.ends_with(']') && line[start..].contains(".csproj]") {
                line.truncate(start);
            }
        }
        line = normalize_path(&line, workspace_root);
        if (line.contains(": error ") || line.contains(": warning ")) && !seen.insert(line.clone())
        {
            continue;
        }
        lines.push(line);
    }
    lines
}

fn is_dotnet_noise(line: &str) -> bool {
    line.starts_with("Welcome to .NET")
        || line.starts_with("Telemetry")
        || line.starts_with("Learn about")
        || line.starts_with("Determining projects to restore")
        || line.starts_with("Restored ")
        || line == "Build succeeded."
        || line.starts_with("Time Elapsed ")
}

/// Condense Godot headless output: drop engine/renderer/audio startup noise,
/// fold `ERROR` + `at: ... (res://path:line)` pairs into `path:line: ERROR`,
/// strip `res://` prefixes, and drop duplicate lines (order preserved).
pub(crate) fn dehydrate_godot_output(raw: &str, workspace_root: &Path) -> Vec<String> {
    let clean = strip_ansi(raw);
    let lines: Vec<&str> = clean
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_godot_noise(line))
        .collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(&line) = lines.get(i) else { break };
        i += 1;
        let mut text = normalize_godot_line(line, workspace_root);
        if is_godot_error(line) {
            if let Some((path, number)) = lines.get(i).and_then(|next| godot_location(next)) {
                let path = normalize_godot_line(&path, workspace_root);
                text = format!("{path}:{number}: {text}");
                i += 1;
            }
        }
        if is_godot_noise(&text) {
            continue;
        }
        if seen.insert(text.clone()) {
            out.push(text);
        }
    }
    out
}

fn is_godot_shutdown_noise(line: &str) -> bool {
    line.contains("Capture not registered:")
        || line.contains("resources still in use at exit")
        || line.contains("ObjectDB instances leaked at exit")
        || line.contains("RID allocations of type")
        || line.contains("pages in use by")
}

fn is_godot_noise(line: &str) -> bool {
    is_godot_shutdown_noise(line)
        || godot_location(line)
            .is_some_and(|(path, _)| path.starts_with("core/") || path.starts_with("servers/"))
        || [
            "Godot Engine v",
            "Vulkan ",
            "OpenGL ",
            "ALSA lib ",
            "PulseAudio",
            "core/",
            "servers/",
            "USER WARNING: ",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
        || (line.starts_with("WARNING: ") && line.contains("lavapipe"))
}

fn is_godot_error(line: &str) -> bool {
    ["SCRIPT ERROR:", "USER ERROR:", "Parse Error:", "ERROR:"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Parse `at: <func> (<path>:<line>)` into `(path, line)`.
fn godot_location(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("at: ")?;
    let inner = rest.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let (path, number) = inner[open + 1..].rsplit_once(':')?;
    if path.is_empty() || number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((path.to_owned(), number.to_owned()))
}

fn normalize_godot_line(line: &str, workspace_root: &Path) -> String {
    normalize_path(&line.replace("res://", ""), workspace_root)
}

/// Godot executable: `ERTANGLE_GODOT_BIN`, else `godot` / `godot4` on `PATH`, else `godot`.
fn resolve_godot_bin() -> String {
    if let Ok(bin) = std::env::var("ERTANGLE_GODOT_BIN") {
        if !bin.trim().is_empty() {
            return bin;
        }
    }
    ["godot", "godot4"]
        .into_iter()
        .find(|name| on_path(name))
        .unwrap_or("godot")
        .to_owned()
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(name);
            candidate.is_file() || candidate.with_extension("exe").is_file()
        })
    })
}

/// Godot may exit 0 even when scripts fail to parse, so inspect the messages too.
fn godot_messages_ok(messages: &[String]) -> bool {
    !messages.iter().any(|line| {
        if is_godot_shutdown_noise(line) {
            return false;
        }
        line.contains("SCRIPT ERROR:")
            || line.contains("Parse Error:")
            || line.contains("Failed to load script")
            || line.contains("ERROR:")
    })
}

/// Inspect gdUnit4 `Statistics` summary lines (`0 errors` and `0 failures`).
fn gdunit4_stats_ok(messages: &[String]) -> bool {
    !messages.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        (lower.contains("errors") && lower.contains("failures"))
            && !(lower.contains("0 errors") && lower.contains("0 failures"))
    })
}

/// Collect changed or untracked `.gd` files in the workspace (skipping `addons/`).
fn changed_gd_files(dir: &Path) -> Vec<String> {
    let mut files = std::collections::BTreeSet::new();
    if let Ok(out) = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "status", "--porcelain"])
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let raw = line.get(3..).unwrap_or("").trim().trim_matches('"');
                let path = raw.rsplit(" -> ").next().unwrap_or(raw);
                if path.ends_with(".gd") && !path.starts_with("addons/") && dir.join(path).is_file()
                {
                    files.insert(path.to_owned());
                }
            }
        }
    }
    if let Ok(out) = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "diff", "--name-only", "HEAD"])
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let path = line.trim();
                if path.ends_with(".gd") && !path.starts_with("addons/") && dir.join(path).is_file()
                {
                    files.insert(path.to_owned());
                }
            }
        }
    }
    files.into_iter().take(20).collect()
}

fn normalize_path(input: &str, root: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_text = root.to_string_lossy();
    let input_text = input.to_owned();
    input_text
        .replace(root_text.as_ref(), "")
        .replace("(/", "(")
        .replace("\\", "/")
        .trim_start_matches('/')
        .to_owned()
}

pub(crate) fn truncate_lines(lines: &[String], max_lines: usize) -> (Vec<String>, usize) {
    let omitted = lines.len().saturating_sub(max_lines);
    (lines.iter().take(max_lines).cloned().collect(), omitted)
}

pub(crate) fn cmd_check(
    cwd: Option<PathBuf>,
    stack: Option<String>,
    quick: bool,
    max_lines: usize,
    json: bool,
) -> Result<()> {
    let dir = cwd.unwrap_or(std::env::current_dir()?);
    let dir = dir
        .canonicalize()
        .with_context(|| format!("cannot access {}", dir.display()))?;
    let stacks = parse_stacks(stack.as_deref(), &dir)?;
    if stacks.is_empty() {
        bail!("no supported project stack detected");
    }
    let mut reports = Vec::new();
    for stack in stacks {
        reports.push(run_stack(stack, &dir, quick, max_lines)?);
    }
    let failed = reports.iter().any(|report| !report.success);
    if json {
        println!("{}", serde_json::to_string(&reports.iter().map(|report| serde_json::json!({"stack":report.stack,"success":report.success,"summary":report.summary,"lines":report.lines})).collect::<Vec<_>>())?);
    } else {
        for report in &reports {
            if report.success {
                println!("PASS [{}]: {}", report.stack, report.summary);
            } else {
                println!(
                    "FAIL [{}] ({} lines): {}",
                    report.stack,
                    report.lines.len(),
                    report.summary
                );
                for line in &report.lines {
                    println!("  {line}");
                }
            }
        }
    }
    if failed {
        bail!("check failed");
    }
    Ok(())
}

fn parse_stacks(explicit: Option<&str>, dir: &Path) -> Result<Vec<ProjectStack>> {
    let Some(explicit) = explicit else {
        return Ok(detect_stacks(dir));
    };
    match explicit.to_ascii_lowercase().as_str() {
        "auto" => Ok(detect_stacks(dir)),
        "rust" => Ok(vec![ProjectStack::Rust]),
        "node" => Ok(vec![ProjectStack::Node]),
        "dotnet" => Ok(vec![ProjectStack::DotNet]),
        "python" => Ok(vec![ProjectStack::Python]),
        "godot" => Ok(vec![ProjectStack::Godot]),
        _ => bail!("unsupported stack: {explicit}"),
    }
}

struct Report {
    stack: &'static str,
    success: bool,
    summary: String,
    lines: Vec<String>,
}

fn run_stack(stack: ProjectStack, dir: &Path, quick: bool, max_lines: usize) -> Result<Report> {
    let start = Instant::now();
    let mut outputs = Vec::new();
    let mut summary = Vec::new();
    let mut success = true;
    match stack {
        ProjectStack::Rust => {
            let clippy = execute(
                dir,
                "cargo",
                &[
                    "clippy",
                    "--all-targets",
                    "--message-format=json",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            let (count, messages) = dehydrate_cargo_json(&combined(&clippy), dir);
            summary.push(format!("clippy ({count} warnings)"));
            outputs.extend(messages);
            success &= clippy.status.success();
            if !quick {
                let test = execute(dir, "cargo", &["test", "--no-fail-fast"])?;
                let (passed, failed, messages) = dehydrate_cargo_test_output(&combined(&test));
                summary.push(format!("test ({passed} passed, {failed} failed)"));
                outputs.extend(messages);
                success &= test.status.success();
            }
        }
        ProjectStack::Node => {
            let manager = if dir.join("pnpm-lock.yaml").exists() {
                "pnpm"
            } else if dir.join("yarn.lock").exists() {
                "yarn"
            } else if dir.join("bun.lock").exists() || dir.join("bun.lockb").exists() {
                "bun"
            } else {
                "npm"
            };
            if dir.join("tsconfig.json").exists() && dir.join("node_modules/.bin/tsc").exists() {
                let typecheck = execute(
                    dir,
                    "npx",
                    &["--no-install", "tsc", "--noEmit", "--pretty", "false"],
                )?;
                outputs.extend(dehydrate_node_output(&combined(&typecheck), dir));
                success &= typecheck.status.success();
                summary.push("typecheck".to_owned());
            }
            if !quick && package_has_test(dir) {
                let args: Vec<&str> = match manager {
                    "pnpm" => vec!["test", "--", "--run"],
                    "yarn" => vec!["test", "--run"],
                    "bun" => vec!["test", "--", "--run"],
                    _ => vec!["test", "--", "--run"],
                };
                let mut args = args;
                if package_script_mentions(dir, "jest") {
                    args = match manager {
                        "pnpm" => vec!["test", "--", "--watchAll=false"],
                        "yarn" => vec!["test", "--watchAll=false"],
                        "bun" => vec!["test", "--", "--watchAll=false"],
                        _ => vec!["test", "--", "--watchAll=false"],
                    };
                }
                let test = execute(dir, manager, &args)?;
                outputs.extend(dehydrate_node_output(&combined(&test), dir));
                success &= test.status.success();
                summary.push("test".to_owned());
            }
        }
        ProjectStack::DotNet => {
            let build = execute(
                dir,
                "dotnet",
                &[
                    "build",
                    "--nologo",
                    "-v",
                    "quiet",
                    "-clp:ErrorsOnly;NoSummary",
                ],
            )?;
            outputs.extend(dehydrate_dotnet_output(&combined(&build), dir));
            success &= build.status.success();
            summary.push("build".to_owned());
            if !quick {
                let test = execute(
                    dir,
                    "dotnet",
                    &["test", "--nologo", "--logger", "console;verbosity=minimal"],
                )?;
                outputs.extend(dehydrate_dotnet_output(&combined(&test), dir));
                success &= test.status.success();
                summary.push("test".to_owned());
            }
        }
        ProjectStack::Python => {
            if !quick {
                let test = execute(dir, "pytest", &["-q", "--tb=short", "--no-header"])?;
                outputs.extend(dehydrate_node_output(&combined(&test), dir));
                success &= test.status.success();
                summary.push("pytest".to_owned());
            }
        }
        ProjectStack::Godot => {
            let godot_bin = resolve_godot_bin();
            let project = dir.to_string_lossy();
            let check = execute(
                dir,
                &godot_bin,
                &["--headless", "--path", &project, "--import"],
            )?;
            let messages = dehydrate_godot_output(&combined(&check), dir);
            success &= check.status.success() && godot_messages_ok(&messages);
            outputs.extend(messages);
            summary.push("headless-import".to_owned());
            let gd_files = changed_gd_files(dir);
            if !gd_files.is_empty() {
                for gd in &gd_files {
                    let sc = execute(
                        dir,
                        &godot_bin,
                        &["--headless", "--path", &project, "--check-only", "-s", gd],
                    )?;
                    let msgs = dehydrate_godot_output(&combined(&sc), dir);
                    success &= sc.status.success() && godot_messages_ok(&msgs);
                    outputs.extend(msgs);
                }
                summary.push(format!("gdscript-check ({})", gd_files.len()));
            }
            if !quick && dir.join("addons/gut/gut_cmdln.gd").is_file() {
                let test = execute(
                    dir,
                    &godot_bin,
                    &[
                        "--headless",
                        "--path",
                        &project,
                        "-s",
                        "addons/gut/gut_cmdln.gd",
                        "-gexit",
                    ],
                )?;
                let messages = dehydrate_godot_output(&combined(&test), dir);
                success &= test.status.success() && godot_messages_ok(&messages);
                outputs.extend(messages);
                summary.push("gut".to_owned());
            }
            if !quick && dir.join("addons/gdUnit4/runtest.sh").is_file() {
                let test = execute_with_env(
                    dir,
                    "sh",
                    &["addons/gdUnit4/runtest.sh", "-a", "test"],
                    &[("GODOT_BIN", &godot_bin)],
                )?;
                let messages = dehydrate_godot_output(&combined(&test), dir);
                success &= test.status.success()
                    && godot_messages_ok(&messages)
                    && gdunit4_stats_ok(&messages);
                outputs.extend(messages);
                summary.push("gdunit4".to_owned());
            }
        }
    }
    if success && outputs.is_empty() {
        outputs.push(format!(
            "{} in {:.1}s",
            summary.join(", "),
            start.elapsed().as_secs_f32()
        ));
    }
    let (lines, omitted) = truncate_lines(&outputs, max_lines);
    let mut summary = summary.join(", ");
    if omitted > 0 {
        summary.push_str(&format!("; {omitted} lines omitted"));
    }
    Ok(Report {
        stack: stack.name(),
        success,
        summary,
        lines,
    })
}

fn execute(dir: &Path, program: &str, args: &[&str]) -> Result<Output> {
    execute_with_env(dir, program, args, &[])
}

fn execute_with_env(
    dir: &Path,
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output()
        .with_context(|| format!("failed to execute {program}"))
}

fn combined(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn package_has_test(dir: &Path) -> bool {
    package_json(dir)
        .and_then(|v| {
            v.pointer("/scripts/test")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some()
}
fn package_script_mentions(dir: &Path, needle: &str) -> bool {
    package_json(dir)
        .and_then(|v| {
            v.pointer("/scripts/test")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|script| script.contains(needle))
}
fn package_json(dir: &Path) -> Option<Value> {
    fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir() -> PathBuf {
        ertangle_test_support::temp_dir("ertangle-check")
    }

    #[test]
    fn detects_project_markers() {
        let root = temp_dir();
        for marker in ["Cargo.toml", "package.json", "App.sln", "pyproject.toml"] {
            fs::write(root.join(marker), "").expect("write marker");
        }
        assert_eq!(
            detect_stacks(&root),
            vec![
                ProjectStack::Rust,
                ProjectStack::Node,
                ProjectStack::DotNet,
                ProjectStack::Python
            ]
        );
        let nested = root.join("src").join("App");
        fs::create_dir_all(&nested).expect("create nested project");
        fs::write(nested.join("App.csproj"), "").expect("write project");
        assert!(detect_stacks(&root).contains(&ProjectStack::DotNet));
    }

    #[test]
    fn strips_csi_and_osc_sequences() {
        assert_eq!(
            strip_ansi("\x1b[31mred\x1b[0m \x1b]0;title\x07ok"),
            "red ok"
        );
    }

    #[test]
    fn dehydrates_and_deduplicates_cargo_messages() {
        let root = temp_dir();
        let diagnostic = r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"dead_code"},"message":"unused","spans":[{"file_name":"src/lib.rs","line_start":3,"column_start":2,"is_primary":true}]}}"#;
        let (count, lines) = dehydrate_cargo_json(&format!("{diagnostic}\n{diagnostic}"), &root);
        assert_eq!(count, 1);
        assert_eq!(lines, vec!["src/lib.rs:3:2: warning[dead_code]: unused"]);
    }

    #[test]
    fn dehydrates_cargo_test_failures() {
        let raw = "test ok_case ... ok\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n---- broken stdout ----\npanicked at src/lib.rs:2\nfailures:\nbroken\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out";
        let (passed, failed, lines) = dehydrate_cargo_test_output(raw);
        assert_eq!((passed, failed), (4, 1));
        assert!(lines.iter().any(|line| line.contains("panicked")));
        assert!(!lines.iter().any(|line| line.contains("ok_case")));
    }

    #[test]
    fn dehydrates_node_output() {
        let root = temp_dir();
        let root_text = root.to_string_lossy();
        let raw = format!("\x1b[31mError\x1b[0m\nat fn ({root_text}/src/main.ts:1:2)\nat fn (node_modules/pkg/a.js:3:4)\n\n vite v5 ready");
        let lines = dehydrate_node_output(&raw, &root);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "at fn (src/main.ts:1:2)");
    }

    #[test]
    fn dehydrates_dotnet_diagnostics() {
        let root = temp_dir();
        let absolute = root.join("src/App.cs");
        let raw = format!("{}(4,5): error CS1061: missing [{}]\n{}(4,5): error CS1061: missing [{}]\nat System.Runtime.CompilerServices.TaskAwaiter.GetResult()\n", absolute.display(), root.join("App.csproj").display(), absolute.display(), root.join("Other.csproj").display());
        let lines = dehydrate_dotnet_output(&raw, &root);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("src/App.cs(4,5): error CS1061"));
        assert!(!lines[0].contains(".csproj"));
    }

    #[test]
    fn detects_and_parses_godot_stack() {
        let root = temp_dir();
        assert!(!detect_stacks(&root).contains(&ProjectStack::Godot));
        fs::write(root.join("project.godot"), "config_version=5\n").expect("write marker");
        assert_eq!(detect_stacks(&root), vec![ProjectStack::Godot]);
        assert_eq!(
            parse_stacks(Some("godot"), &root).expect("parse godot"),
            vec![ProjectStack::Godot]
        );
        assert_eq!(ProjectStack::Godot.name(), "godot");
    }

    #[test]
    fn dehydrates_godot_output() {
        let root = temp_dir();
        let raw =
            "\x1b[1mGodot Engine v4.3.stable.official.77dcf97d8 - https://godotengine.org\x1b[0m\n\
Vulkan API 1.3.255 - Forward+ - Using Device #0: llvmpipe\n\
\n\
SCRIPT ERROR: Parse Error: Identifier \"foo\" not declared in the current scope.\n\
          at: GDScript::reload (res://scripts/player.gd:42)\n\
SCRIPT ERROR: Parse Error: Identifier \"foo\" not declared in the current scope.\n\
          at: GDScript::reload (res://scripts/player.gd:42)\n\
ERROR: Failed to load script \"res://scripts/player.gd\" with error \"Parse error\".\n\
   at: load (modules/gdscript/gdscript.cpp:2936)\n";
        let lines = dehydrate_godot_output(raw, &root);
        assert_eq!(
            lines,
            vec![
                "scripts/player.gd:42: SCRIPT ERROR: Parse Error: Identifier \"foo\" not declared in the current scope.",
                "modules/gdscript/gdscript.cpp:2936: ERROR: Failed to load script \"scripts/player.gd\" with error \"Parse error\".",
            ]
        );
        assert!(!godot_messages_ok(&lines));
        assert!(godot_messages_ok(&["all good".to_owned()]));
    }

    #[test]
    fn godot_filters_shutdown_noise_and_checks_gdunit4_stats() {
        let root = temp_dir();
        let raw = "\
ERROR: Capture not registered: 'beehave'\n\
   at: capture (core/debugger/engine_debugger.cpp:62)\n\
core/io/resource.cpp:810: ERROR: 3 resources still in use at exit\n\
WARNING: ObjectDB instances leaked at exit\n\
RID allocations of type 'N10RendererRD' leaked\n\
| 0 errors | 0 failures |\n";
        let lines = dehydrate_godot_output(raw, &root);
        assert_eq!(lines, vec!["| 0 errors | 0 failures |"]);
        assert!(godot_messages_ok(&lines));
        assert!(gdunit4_stats_ok(&lines));
        assert!(!gdunit4_stats_ok(&["| 1 errors | 0 failures |".to_owned()]));
        assert!(godot_messages_ok(&[
            "core/debugger/engine_debugger.cpp:62: ERROR: Capture not registered: 'beehave'"
                .to_owned(),
            "WARNING: ObjectDB instances leaked at exit".to_owned(),
        ]));
        assert!(!godot_messages_ok(&[
            "scripts/a.gd:10: SCRIPT ERROR: Invalid call".to_owned(),
        ]));
    }

    #[test]
    fn godot_location_requires_numeric_line() {
        assert_eq!(
            godot_location("at: f (res://a.gd:7)"),
            Some(("res://a.gd".to_owned(), "7".to_owned()))
        );
        assert_eq!(godot_location("at: f (res://a.gd:x)"), None);
        assert_eq!(godot_location("not a location"), None);
    }

    #[test]
    fn truncates_with_omission_count() {
        let lines = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(
            truncate_lines(&lines, 2),
            (vec!["a".to_owned(), "b".to_owned()], 1)
        );
    }
}
