#![allow(dead_code)]
//! `ertangle outline`: fast polyglot code skeleton (Rust, TS/JS/Vue, C#, Python).
//!
//! A dependency-free line scanner: comments and string literals are blanked out
//! first, then declarations are matched per line and their end line is found by
//! bracket depth (or indentation for Python).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// One declaration found in a source file (1-based, inclusive line range).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolEntry {
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
}

impl SymbolEntry {
    fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "signature": self.signature,
            "start_line": self.start_line,
            "end_line": self.end_line,
        })
    }
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "bin",
    "obj",
    ".ertangle",
    "dist",
];
const DEFAULT_MAX_DEPTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Ts,
    Cs,
    Py,
}

fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_str()? {
        "rs" => Some(Lang::Rust),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "vue" => Some(Lang::Ts),
        "cs" => Some(Lang::Cs),
        "py" => Some(Lang::Py),
        _ => None,
    }
}

/// Extract declarations (with exact line ranges) from `content`, language chosen by extension.
pub(crate) fn extract_outline(path: &Path, content: &str) -> Vec<SymbolEntry> {
    match lang_of(path) {
        Some(Lang::Py) => extract_python(content),
        Some(lang) => extract_braced(lang, content),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------- sanitizer

#[derive(Clone, Copy, PartialEq)]
enum State {
    Normal,
    Block,
    /// Rust `"..."` (may span lines).
    DqMulti,
    /// TS template literal.
    Backtick,
    /// C# `@"..."`.
    Verbatim,
}

/// Replace comments and string/char literal contents with spaces (char-for-char),
/// so bracket counting and keyword matching only see real code.
fn sanitize(content: &str, lang: Lang) -> Vec<String> {
    let mut state = State::Normal;
    let mut out = Vec::new();
    for line in content.lines() {
        let c: Vec<char> = line.chars().collect();
        let mut o = String::with_capacity(line.len());
        let mut i = 0;
        while i < c.len() {
            let ch = c[i];
            let next = c.get(i + 1).copied();
            match state {
                State::Block => {
                    if ch == '*' && next == Some('/') {
                        o.push_str("  ");
                        i += 2;
                        state = State::Normal;
                    } else {
                        o.push(' ');
                        i += 1;
                    }
                }
                State::DqMulti | State::Backtick => {
                    let close = if state == State::DqMulti { '"' } else { '`' };
                    if ch == '\\' {
                        o.push(' ');
                        if next.is_some() {
                            o.push(' ');
                        }
                        i += 2;
                    } else if ch == close {
                        o.push(ch);
                        i += 1;
                        state = State::Normal;
                    } else {
                        o.push(' ');
                        i += 1;
                    }
                }
                State::Verbatim => {
                    if ch == '"' && next == Some('"') {
                        o.push_str("  ");
                        i += 2;
                    } else if ch == '"' {
                        o.push('"');
                        i += 1;
                        state = State::Normal;
                    } else {
                        o.push(' ');
                        i += 1;
                    }
                }
                State::Normal => {
                    if ch == '/' && next == Some('/') {
                        o.extend(std::iter::repeat_n(' ', c.len() - i));
                        break;
                    }
                    if ch == '/' && next == Some('*') {
                        o.push_str("  ");
                        i += 2;
                        state = State::Block;
                        continue;
                    }
                    match ch {
                        '"' if lang == Lang::Rust => {
                            o.push('"');
                            i += 1;
                            state = State::DqMulti;
                        }
                        '"' if lang == Lang::Cs && i > 0 && c[i - 1] == '@' => {
                            o.push('"');
                            i += 1;
                            state = State::Verbatim;
                        }
                        '"' => i = blank_inline(&c, i, '"', &mut o),
                        '\'' if lang == Lang::Ts => i = blank_inline(&c, i, '\'', &mut o),
                        '\'' => i = blank_char_literal(&c, i, &mut o),
                        '`' if lang == Lang::Ts => {
                            o.push('`');
                            i += 1;
                            state = State::Backtick;
                        }
                        _ => {
                            o.push(ch);
                            i += 1;
                        }
                    }
                }
            }
        }
        out.push(o);
    }
    out
}

/// Blank a single-line quoted string starting at `start`; returns the next index.
fn blank_inline(c: &[char], start: usize, quote: char, o: &mut String) -> usize {
    o.push(quote);
    let mut i = start + 1;
    while i < c.len() {
        if c[i] == '\\' {
            o.push(' ');
            if i + 1 < c.len() {
                o.push(' ');
            }
            i += 2;
        } else if c[i] == quote {
            o.push(quote);
            return i + 1;
        } else {
            o.push(' ');
            i += 1;
        }
    }
    i
}

/// Rust/C# char literal (`'x'`, `'\n'`); a lone `'` (lifetime) is kept as-is.
fn blank_char_literal(c: &[char], start: usize, o: &mut String) -> usize {
    let is_lit = matches!(
        (c.get(start + 1), c.get(start + 2)),
        (Some('\\'), _) | (Some(_), Some('\''))
    );
    if is_lit {
        blank_inline(c, start, '\'', o)
    } else {
        o.push('\'');
        start + 1
    }
}

// ---------------------------------------------------------------- end finder

/// Index of the last line of the declaration starting at `start`.
/// Ends at a `}` returning to depth 0 or a `;` at depth 0. With `eol_rule`
/// (TS `const`) a complete line at depth 0 also ends it.
fn find_end(clean: &[String], start: usize, eol_rule: bool) -> usize {
    let mut depth: usize = 0;
    for (idx, line) in clean.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' | '(' | '[' => depth += 1,
                '}' | ')' | ']' => {
                    depth = depth.saturating_sub(1);
                    if ch == '}' && depth == 0 {
                        return idx;
                    }
                }
                ';' if depth == 0 => return idx,
                _ => {}
            }
        }
        if eol_rule && depth == 0 {
            let t = line.trim_end();
            let continues = [
                "=", "=>", "(", ",", "&&", "||", "+", ":", "?", "|", "&", "{", "<",
            ]
            .iter()
            .any(|s| t.ends_with(s));
            if !t.is_empty() && !continues {
                return idx;
            }
        }
    }
    clean.len().saturating_sub(1)
}

/// Original signature text: cut at first `{` outside parens, drop trailing `;`.
fn signature_of(clean: &str, orig: &str) -> String {
    let oc: Vec<char> = orig.chars().collect();
    let mut depth = 0usize;
    let mut cut = oc.len();
    for (i, ch) in clean.chars().enumerate() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => {
                cut = i;
                break;
            }
            _ => {}
        }
    }
    let s: String = oc[..cut.min(oc.len())].iter().collect();
    s.trim().trim_end_matches(';').trim().to_string()
}

// ---------------------------------------------------------------- detection

struct Decl {
    kind: &'static str,
    /// Containers do not skip their body; leaves do.
    container: bool,
    /// Declaration is a type (C#) so members may follow.
    is_type: bool,
    eol_rule: bool,
}

fn take_word(s: &str) -> (&str, &str) {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (&s[..end], &s[end..])
}

fn starts_ws_or(after: &str, extra: &[char]) -> bool {
    after
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || extra.contains(&c))
}

fn detect_rust(line: &str) -> Option<Decl> {
    let mut rest = line.trim_start();
    if let Some(r) = rest.strip_prefix("pub") {
        if r.starts_with(|c: char| c.is_whitespace() || c == '(') {
            rest = r.trim_start();
            if rest.starts_with('(') {
                rest = rest.find(')').map(|p| &rest[p + 1..]).unwrap_or("");
            }
        }
    }
    loop {
        rest = rest.trim_start();
        let (w, after) = take_word(rest);
        match w {
            "async" | "const" | "unsafe" | "extern" | "default" => {
                rest = after.trim_start();
                if rest.starts_with('"') {
                    rest = rest[1..].find('"').map(|p| &rest[p + 2..]).unwrap_or("");
                }
            }
            "mod" | "trait" if starts_ws_or(after, &[]) => {
                return Some(decl(if w == "mod" { "mod" } else { "trait" }, true, false))
            }
            "impl" if starts_ws_or(after, &['<']) => return Some(decl("impl", true, false)),
            "struct" | "enum" | "union" | "type" | "fn" if starts_ws_or(after, &[]) => {
                let kind = match w {
                    "struct" => "struct",
                    "enum" => "enum",
                    "union" => "union",
                    "type" => "type",
                    _ => "fn",
                };
                return Some(decl(kind, false, false));
            }
            _ => return None,
        }
    }
}

fn decl(kind: &'static str, container: bool, is_type: bool) -> Decl {
    Decl {
        kind,
        container,
        is_type,
        eol_rule: false,
    }
}

fn detect_ts(line: &str) -> Option<Decl> {
    let mut rest = line.trim_start();
    let mut exported = false;
    loop {
        rest = rest.trim_start();
        let (w, after) = take_word(rest);
        match w {
            "export" => {
                exported = true;
                rest = after;
            }
            "default" | "declare" | "abstract" | "async" => rest = after,
            "interface" | "class" | "enum" if starts_ws_or(after, &[]) => {
                let kind = match w {
                    "interface" => "interface",
                    "class" => "class",
                    _ => "enum",
                };
                return Some(decl(kind, false, false));
            }
            "function" if starts_ws_or(after, &['*', '(']) => {
                return Some(decl("function", false, false))
            }
            "type" if starts_ws_or(after, &[]) => {
                let (name, tail) = take_word(after.trim_start());
                let t = tail.trim_start();
                if !name.is_empty() && (t.starts_with('=') || t.starts_with('<')) {
                    return Some(decl("type", false, false));
                }
                return None;
            }
            "const" if exported && starts_ws_or(after, &[]) => {
                let (name, _) = take_word(after.trim_start());
                if name == "enum" {
                    return Some(decl("enum", false, false));
                }
                if name.is_empty() {
                    return None;
                }
                return Some(Decl {
                    kind: "const",
                    container: false,
                    is_type: false,
                    eol_rule: true,
                });
            }
            "let" | "var" if exported && starts_ws_or(after, &[]) => {
                return Some(Decl {
                    kind: "const",
                    container: false,
                    is_type: false,
                    eol_rule: true,
                })
            }
            _ => return None,
        }
    }
}

const CS_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "abstract",
    "sealed",
    "partial",
    "virtual",
    "override",
    "async",
    "new",
    "readonly",
    "unsafe",
    "extern",
    "file",
    "required",
    "ref",
];
const CS_STMT_WORDS: &[&str] = &[
    "if", "for", "foreach", "while", "switch", "return", "using", "lock", "catch", "else", "throw",
    "await", "var", "new", "base", "this", "do", "try", "fixed", "yield", "case", "break",
    "continue", "goto", "typeof", "sizeof", "nameof",
];

fn detect_cs(line: &str, in_type: bool) -> Option<Decl> {
    let mut rest = line.trim_start();
    loop {
        let (w, after) = take_word(rest);
        if CS_MODIFIERS.contains(&w) && starts_ws_or(after, &[]) {
            rest = after.trim_start();
        } else {
            break;
        }
    }
    let (w, after) = take_word(rest);
    let named = starts_ws_or(after, &[]);
    match w {
        "namespace" if named => return Some(decl("namespace", true, false)),
        "class" if named => return Some(decl("class", true, true)),
        "interface" if named => return Some(decl("interface", true, true)),
        "struct" if named => return Some(decl("struct", true, true)),
        "record" if named => return Some(decl("record", true, true)),
        "enum" if named => return Some(decl("enum", false, false)),
        _ => {}
    }
    if !in_type {
        return None;
    }
    cs_method(rest).then(|| decl("method", false, false))
}

/// Does `rest` (modifiers removed) look like `Type Name(` or `Name(`?
fn cs_method(rest: &str) -> bool {
    let Some(paren) = rest.find('(') else {
        return false;
    };
    let mut prefix = String::new();
    let mut gen = 0usize;
    for ch in rest[..paren].chars() {
        match ch {
            '<' => gen += 1,
            '>' => gen = gen.saturating_sub(1),
            _ if gen > 0 => {}
            _ => prefix.push(ch),
        }
    }
    let ok_chars = prefix
        .chars()
        .all(|c| c.is_alphanumeric() || c.is_whitespace() || "_.?[],~".contains(c));
    let tokens: Vec<&str> = prefix.split_whitespace().collect();
    if !ok_chars || tokens.is_empty() || tokens.len() > 2 {
        return false;
    }
    if CS_STMT_WORDS.contains(&tokens[0]) {
        return false;
    }
    tokens.len() == 2 || tokens[0].starts_with(|c: char| c.is_uppercase() || c == '~')
}

// ---------------------------------------------------------------- braced langs

fn extract_braced(lang: Lang, content: &str) -> Vec<SymbolEntry> {
    let originals: Vec<&str> = content.lines().collect();
    let clean = sanitize(content, lang);
    let n = clean.len();
    let mut out = Vec::new();
    // (end_idx, is_type)
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut next_scan = 0usize;
    let mut pending_attrs: Vec<String> = Vec::new();
    let mut pending_start: Option<usize> = None;

    for i in 0..n {
        if i < next_scan {
            continue;
        }
        while stack.last().is_some_and(|(end, _)| *end < i) {
            stack.pop();
        }
        let oc: Vec<char> = originals[i].chars().collect();
        let cc: Vec<char> = clean[i].chars().collect();
        let mut off = cc.iter().take_while(|c| c.is_whitespace()).count();
        if off >= cc.len() {
            continue; // blank or comment-only: keep pending attributes
        }
        if lang == Lang::Cs {
            while cc.get(off) == Some(&'[') {
                let Some(close) = matching_bracket(&cc, off) else {
                    break;
                };
                let attr: String = oc[off..=close].iter().collect();
                if ["[Http", "[Route"].iter().any(|p| attr.starts_with(p)) {
                    pending_attrs.push(attr);
                    pending_start.get_or_insert(i);
                }
                off = close + 1;
                while cc.get(off).is_some_and(|c| c.is_whitespace()) {
                    off += 1;
                }
            }
            if off >= cc.len() {
                continue;
            }
        }
        let text: String = cc[off..].iter().collect();
        let in_type = stack.last().is_some_and(|(_, t)| *t);
        let found = match lang {
            Lang::Rust => detect_rust(&text),
            Lang::Ts => detect_ts(&text),
            _ => detect_cs(&text, in_type),
        };
        let Some(d) = found else {
            pending_attrs.clear();
            pending_start = None;
            continue;
        };
        let orig_text: String = oc[off.min(oc.len())..].iter().collect();
        let mut end = find_end(&clean, i, d.eol_rule);
        if d.kind == "namespace" && text.trim_end().ends_with(';') {
            end = n - 1; // file-scoped namespace spans to EOF
        }
        let mut signature = signature_of(&text, &orig_text);
        if !pending_attrs.is_empty() {
            signature = format!("{} {}", pending_attrs.join(" "), signature);
        }
        out.push(SymbolEntry {
            kind: d.kind.to_string(),
            signature,
            start_line: pending_start.unwrap_or(i) + 1,
            end_line: end + 1,
        });
        pending_attrs.clear();
        pending_start = None;
        if d.container {
            stack.push((end, d.is_type));
        } else {
            next_scan = end + 1;
        }
    }
    out
}

fn matching_bracket(cc: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in cc.iter().enumerate().skip(open) {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------- python

fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

fn extract_python(content: &str) -> Vec<SymbolEntry> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let kind = if t.starts_with("class ") {
            "class"
        } else if t.starts_with("def ") || t.starts_with("async def ") {
            "def"
        } else {
            continue;
        };
        let indent = indent_of(line);
        let mut balance = paren_balance(t);
        let mut last = i;
        let mut j = i + 1;
        // Multi-line signature: continue until parentheses balance.
        while balance > 0 && j < lines.len() {
            balance += paren_balance(lines[j]);
            last = j;
            j += 1;
        }
        while j < lines.len() {
            let l = lines[j];
            let lt = l.trim();
            if lt.is_empty() || lt.starts_with('#') {
                j += 1;
                continue;
            }
            if indent_of(l) <= indent {
                break;
            }
            last = j;
            j += 1;
        }
        out.push(SymbolEntry {
            kind: kind.to_string(),
            signature: t.trim().trim_end_matches(':').trim().to_string(),
            start_line: i + 1,
            end_line: last + 1,
        });
    }
    out
}

fn paren_balance(s: &str) -> i32 {
    s.chars().fold(0, |b, c| match c {
        '(' | '[' | '{' => b + 1,
        ')' | ']' | '}' => b - 1,
        _ => b,
    })
}

// ---------------------------------------------------------------- output

/// Render a single file outline: header plus `  L<start>-L<end>: <signature>` lines,
/// indented two extra spaces per nesting level.
pub(crate) fn render_file(path: &Path, content: &str) -> String {
    let entries = extract_outline(path, content);
    let mut s = format!("{} ({} lines):\n", path.display(), content.lines().count());
    let mut ends: Vec<usize> = Vec::new();
    for e in &entries {
        while ends.last().is_some_and(|end| *end < e.start_line) {
            ends.pop();
        }
        let pad = "  ".repeat(ends.len() + 1);
        s.push_str(&format!(
            "{pad}L{}-L{}: {}\n",
            e.start_line, e.end_line, e.signature
        ));
        ends.push(e.end_line);
    }
    s
}

/// Name token following the kind keyword in a signature (for compact summaries).
fn symbol_name(e: &SymbolEntry) -> String {
    let mut toks = e.signature.split_whitespace();
    let kw = if e.kind == "method" || e.kind == "const" {
        None
    } else {
        Some(e.kind.as_str())
    };
    if let Some(kw) = kw {
        for t in toks.by_ref() {
            if t == kw {
                break;
            }
        }
        return toks
            .next()
            .map(|t| take_word(t).0.to_string())
            .unwrap_or_default();
    }
    let sig = e.signature.split('(').next().unwrap_or("");
    sig.split_whitespace()
        .last()
        .map(|t| take_word(t).0.to_string())
        .unwrap_or_default()
}

fn render_summary(path: &Path, content: &str) -> String {
    let entries = extract_outline(path, content);
    let tops: Vec<String> = entries
        .iter()
        .filter(|e| !matches!(e.kind.as_str(), "fn" | "method" | "def" | "function"))
        .take(5)
        .map(|e| format!("{} {}", e.kind, symbol_name(e)))
        .collect();
    format!(
        "{} ({} lines): {} symbols{}\n",
        path.display(),
        content.lines().count(),
        entries.len(),
        if tops.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tops.join(", "))
        }
    )
}

/// Collect supported source files below `dir`, descending at most `max_depth` levels.
pub(crate) fn collect_files(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, 0, max_depth, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if depth < max_depth && !SKIP_DIRS.contains(&name) {
                walk(&p, depth + 1, max_depth, out);
            }
        } else if lang_of(&p).is_some() {
            out.push(p);
        }
    }
}

/// CLI entrypoint for `ertangle outline`.
pub(crate) fn cmd_outline(paths: Vec<PathBuf>, max_depth: Option<usize>, json: bool) -> Result<()> {
    if paths.is_empty() {
        bail!("usage: ertangle outline <path>... [--max-depth N] [--json]");
    }
    let depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    let mut json_files = Vec::new();
    for p in &paths {
        if !p.exists() {
            bail!("path not found: {}", p.display());
        }
        let is_dir = p.is_dir();
        let files = if is_dir {
            collect_files(p, depth)
        } else {
            vec![p.clone()]
        };
        for f in files {
            if lang_of(&f).is_none() {
                println!("{}: unsupported language", f.display());
                continue;
            }
            let content = match std::fs::read_to_string(&f) {
                Ok(c) => c,
                Err(_) if is_dir => continue,
                Err(e) => return Err(e).with_context(|| format!("read {}", f.display())),
            };
            if json {
                let syms: Vec<Value> = extract_outline(&f, &content)
                    .iter()
                    .map(SymbolEntry::to_json)
                    .collect();
                json_files.push(json!({
                    "path": f.display().to_string(),
                    "lines": content.lines().count(),
                    "symbols": syms,
                }));
            } else if is_dir {
                print!("{}", render_summary(&f, &content));
            } else {
                print!("{}", render_file(&f, &content));
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&json_files)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(path: &str, src: &str) -> Vec<(String, usize, usize)> {
        extract_outline(Path::new(path), src)
            .into_iter()
            .map(|e| (e.signature, e.start_line, e.end_line))
            .collect()
    }

    #[test]
    fn rust_items_and_ranges() {
        let src = r#"use std::fmt;

pub struct Foo {
    a: u32, // } in comment
    s: &'static str,
}

pub(crate) struct Unit;

impl Foo {
    pub async fn go(&self) -> u32 {
        let s = "}{";
        let c = '}';
        /* { */
        1
    }
    const fn k() -> u32 { 2 }
}

pub enum E {
    A,
}

trait T {
    fn m(&self);
}
type Alias = Vec<u8>;
mod inner {
    fn deep() {}
}
"#;
        let r = ranges("a.rs", src);
        assert!(r.contains(&("pub struct Foo".into(), 3, 6)));
        assert!(r.contains(&("pub(crate) struct Unit".into(), 8, 8)));
        assert!(r.contains(&("impl Foo".into(), 10, 18)));
        assert!(r.contains(&("pub async fn go(&self) -> u32".into(), 11, 16)));
        assert!(r.contains(&("const fn k() -> u32".into(), 17, 17)));
        assert!(r.contains(&("pub enum E".into(), 20, 22)));
        assert!(r.contains(&("trait T".into(), 24, 26)));
        assert!(r.contains(&("fn m(&self)".into(), 25, 25)));
        assert!(r.contains(&("type Alias = Vec<u8>".into(), 27, 27)));
        assert!(r.contains(&("mod inner".into(), 28, 30)));
        assert!(r.contains(&("fn deep()".into(), 29, 29)));
    }

    #[test]
    fn rust_nested_fn_in_body_is_skipped() {
        let src = "fn outer() {\n    fn inner() {}\n}\n";
        let r = ranges("a.rs", src);
        assert_eq!(r, vec![("fn outer()".to_string(), 1, 3)]);
    }

    #[test]
    fn typescript_items_and_ranges() {
        let src = r#"import x from 'y';

export interface Props {
  a: string;
}

export type Id = string;
export type Obj = {
  k: number;
};

export function foo(a: { b: number }): void {
  const s = "}";
}

export const Comp = (props: Props) => {
  return `}`;
};

export const hook = () =>
  useThing();

export default class Foo {
  m() {}
}

export enum Color { Red }
"#;
        let r = ranges("a.tsx", src);
        assert!(r.contains(&("export interface Props".into(), 3, 5)));
        assert!(r.contains(&("export type Id = string".into(), 7, 7)));
        assert!(r.contains(&("export type Obj =".into(), 8, 10)));
        assert!(r.contains(&("export function foo(a: { b: number }): void".into(), 12, 14)));
        assert!(r.contains(&("export const Comp = (props: Props) =>".into(), 16, 18)));
        assert!(r.contains(&("export const hook = () =>".into(), 20, 21)));
        assert!(r.contains(&("export default class Foo".into(), 23, 25)));
        assert!(r.contains(&("export enum Color".into(), 27, 27)));
    }

    #[test]
    fn csharp_items_attributes_and_ranges() {
        let src = r#"namespace App.Api
{
    [ApiController]
    public class OrdersController : ControllerBase
    {
        private readonly int _x = Make();

        public OrdersController(int x)
        {
            _x = x;
        }

        [HttpPost("orders")]
        public async Task<IActionResult> CreateOrder(Dto d)
        {
            if (d == null) { return BadRequest(); }
            return Ok();
        }

        public int Count => Items.Count();
    }

    public record Dto(string Name);
    public interface IFoo { void Bar(); }
    public enum Kind { A, B }
}
"#;
        let r = ranges("a.cs", src);
        assert!(r.contains(&("namespace App.Api".into(), 1, 26)));
        assert!(r.contains(&(
            "public class OrdersController : ControllerBase".into(),
            4,
            21
        )));
        assert!(r.contains(&("public OrdersController(int x)".into(), 8, 11)));
        assert!(r.contains(&(
            "[HttpPost(\"orders\")] public async Task<IActionResult> CreateOrder(Dto d)".into(),
            13,
            18
        )));
        assert!(r.contains(&("public record Dto(string Name)".into(), 23, 23)));
        assert!(r.contains(&("public interface IFoo".into(), 24, 24)));
        assert!(r.contains(&("public enum Kind".into(), 25, 25)));
        // statements and properties are not methods
        assert!(!r
            .iter()
            .any(|(s, _, _)| s.contains("Make") || s.contains("Count")));
    }

    #[test]
    fn csharp_file_scoped_namespace() {
        let src = "namespace Foo;\n\npublic class A\n{\n    void M() {}\n}\n";
        let r = ranges("a.cs", src);
        assert!(r.contains(&("namespace Foo".into(), 1, 6)));
        assert!(r.contains(&("public class A".into(), 3, 6)));
        assert!(r.contains(&("void M()".into(), 5, 5)));
    }

    #[test]
    fn python_items_and_ranges() {
        let src = r#"import os

class Foo:
    """doc"""

    def a(self):
        return 1

    # comment

    async def b(self,
                x):
        pass


def top():
    pass
"#;
        let r = ranges("a.py", src);
        assert!(r.contains(&("class Foo".into(), 3, 13)));
        assert!(r.contains(&("def a(self)".into(), 6, 7)));
        assert!(r.contains(&("async def b(self,".into(), 11, 13)));
        assert!(r.contains(&("def top()".into(), 16, 17)));
    }

    #[test]
    fn render_indents_nested_and_header() {
        let src = "struct S;\nimpl S {\n    fn f(&self) {}\n}\n";
        let out = render_file(Path::new("x.rs"), src);
        assert_eq!(
            out,
            "x.rs (4 lines):\n  L1-L1: struct S\n  L2-L4: impl S\n    L3-L3: fn f(&self)\n"
        );
    }

    #[test]
    fn unsupported_extension_is_empty() {
        assert!(extract_outline(Path::new("a.txt"), "fn x() {}").is_empty());
    }

    #[test]
    fn directory_walk_respects_depth_and_skips() {
        let root = ertangle_test_support::temp_dir("ertangle-outline");
        let deep = root.join("a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("top.py"), "def f():\n    pass\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        std::fs::write(
            root.join("node_modules/skip.js"),
            "export function z() {}\n",
        )
        .unwrap();
        std::fs::write(deep.join("deep.rs"), "fn d() {}\n").unwrap();
        std::fs::write(root.join("note.txt"), "x").unwrap();

        let files = collect_files(&root, 3);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        assert_eq!(names, vec!["src/lib.rs", "top.py"]);
        assert!(collect_files(&root, 4)
            .iter()
            .any(|p| p.ends_with("a/b/c/d/deep.rs")));

        let summary = render_summary(&root.join("top.py"), "def f():\n    pass\n");
        assert!(summary.contains("(2 lines): 1 symbols"));
    }

    #[test]
    fn cmd_outline_errors() {
        assert!(cmd_outline(vec![], None, false).is_err());
        assert!(cmd_outline(vec![PathBuf::from("/nonexistent/zzz.rs")], None, false).is_err());
    }
}
