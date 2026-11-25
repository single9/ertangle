#![allow(dead_code)]
//! `ertangle outline`: fast polyglot code skeleton (Rust, TS/JS/Vue, C#, Python,
//! GDScript, Godot scenes and shaders).
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
    ".godot",
];
const DEFAULT_MAX_DEPTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Ts,
    Cs,
    Py,
    Gd,
    Tscn,
    GdShader,
}

fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_str()? {
        "rs" => Some(Lang::Rust),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "vue" => Some(Lang::Ts),
        "cs" => Some(Lang::Cs),
        "py" => Some(Lang::Py),
        "gd" => Some(Lang::Gd),
        "tscn" => Some(Lang::Tscn),
        "gdshader" => Some(Lang::GdShader),
        _ => None,
    }
}

/// Extract declarations (with exact line ranges) from `content`, language chosen by extension.
pub(crate) fn extract_outline(path: &Path, content: &str) -> Vec<SymbolEntry> {
    match lang_of(path) {
        Some(Lang::Py) => extract_python(content),
        Some(Lang::Gd) => extract_gdscript(content),
        Some(Lang::Tscn) => extract_tscn(content),
        Some(Lang::GdShader) => extract_gdshader(content),
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

// ---------------------------------------------------------------- godot

/// Line index after the indented body of a block starting at `i` (Python/GDScript rule):
/// returns the index of the last line belonging to the block.
fn indented_block_end(lines: &[&str], i: usize) -> usize {
    let indent = indent_of(lines[i]);
    let mut balance = paren_balance(lines[i]);
    let mut last = i;
    let mut j = i + 1;
    // Multi-line signature: continue until brackets balance.
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
    last
}

/// Drop a long `= value` initializer so the signature stays compact.
fn strip_long_value(sig: &str) -> String {
    const MAX_SIG: usize = 60;
    if sig.chars().count() <= MAX_SIG {
        return sig.to_string();
    }
    match sig.split_once('=') {
        Some((head, _)) => head.trim_end().trim_end_matches(':').trim_end().to_string(),
        None => sig.to_string(),
    }
}

fn single(kind: &str, signature: String, idx: usize) -> SymbolEntry {
    SymbolEntry {
        kind: kind.to_string(),
        signature,
        start_line: idx + 1,
        end_line: idx + 1,
    }
}

/// GDScript (`.gd`): indentation-based like Python, `#` comments.
fn extract_gdscript(content: &str) -> Vec<SymbolEntry> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let top_level = indent_of(line) == 0;
        let word = take_word(t).0;
        let entry = match word {
            "class_name" | "extends" | "signal" if starts_ws_or(&t[word.len()..], &[]) => {
                single(word, t.to_string(), i)
            }
            "const" if starts_ws_or(&t[word.len()..], &[]) => {
                single("const", strip_long_value(t), i)
            }
            "var" if top_level && starts_ws_or(&t[3..], &[]) => {
                single("var", strip_long_value(t), i)
            }
            _ if t.starts_with('@') && gd_annotated_var(t) => single("var", strip_long_value(t), i),
            "enum" if starts_ws_or(&t[4..], &['{']) => {
                let mut balance = paren_balance(t);
                let mut last = i;
                while balance > 0 && last + 1 < lines.len() {
                    last += 1;
                    balance += paren_balance(lines[last]);
                }
                SymbolEntry {
                    kind: "enum".to_string(),
                    signature: gd_enum_signature(t),
                    start_line: i + 1,
                    end_line: last + 1,
                }
            }
            "class" if starts_ws_or(&t[5..], &[]) => gd_block("class", t, &lines, i),
            "func" if starts_ws_or(&t[4..], &[]) => gd_block("func", t, &lines, i),
            "static" if t["static".len()..].trim_start().starts_with("func ") => {
                gd_block("func", t, &lines, i)
            }
            _ => continue,
        };
        out.push(entry);
    }
    out
}

/// `@export var x`, `@onready var x`, `@export_range(0, 1) var x` (annotation(s) then `var`).
fn gd_annotated_var(t: &str) -> bool {
    t.split_whitespace().any(|tok| tok == "var")
        && t.split_whitespace()
            .take_while(|tok| *tok != "var")
            .all(|tok| tok.starts_with('@') || !tok.starts_with(|c: char| c.is_alphabetic()))
}

/// `enum State { IDLE, RUN }` -> `enum State`; anonymous enums keep `enum`.
fn gd_enum_signature(t: &str) -> String {
    t.split('{').next().unwrap_or(t).trim().to_string()
}

fn gd_block(kind: &str, t: &str, lines: &[&str], i: usize) -> SymbolEntry {
    let end = indented_block_end(lines, i);
    SymbolEntry {
        kind: kind.to_string(),
        signature: gd_header(t),
        start_line: i + 1,
        end_line: end + 1,
    }
}

/// Header text up to the block-opening `:` (first `:` outside brackets), so
/// `func f(x: int) -> void: pass` becomes `func f(x: int) -> void`.
fn gd_header(t: &str) -> String {
    let mut depth = 0i32;
    for (idx, ch) in t.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 => return t[..idx].trim().to_string(),
            _ => {}
        }
    }
    t.trim().to_string()
}

/// Godot scene (`.tscn`): keep `[node]`, `[connection]` and script/scene `[ext_resource]`
/// sections; skip `[gd_scene]`, `[sub_resource]`, `[editable]` and other bulky data.
fn extract_tscn(content: &str) -> Vec<SymbolEntry> {
    let lines: Vec<&str> = content.lines().collect();
    let headers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| tscn_section_kind(l).is_some())
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    for (n, &start) in headers.iter().enumerate() {
        let header = lines[start].trim();
        let Some(kind) = tscn_section_kind(header) else {
            continue;
        };
        let keep = match kind {
            "node" | "connection" => true,
            "ext_resource" => {
                header.contains("type=\"Script\"") || header.contains("type=\"PackedScene\"")
            }
            _ => false,
        };
        if !keep {
            continue;
        }
        let next = headers.get(n + 1).copied().unwrap_or(lines.len());
        let end = (start..next)
            .rev()
            .find(|&j| !lines[j].trim().is_empty())
            .unwrap_or(start);
        out.push(SymbolEntry {
            kind: kind.to_string(),
            signature: header.to_string(),
            start_line: start + 1,
            end_line: end + 1,
        });
    }
    out
}

/// Section kind of a `[kind attr=...]` header line, if `line` is one.
fn tscn_section_kind(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (kind, rest) = take_word(inner);
    let valid = !kind.is_empty()
        && kind.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        && (rest.is_empty() || rest.starts_with(' '));
    valid.then_some(kind)
}

/// Godot shader (`.gdshader`): C-like syntax with `//` and `/* */` comments.
fn extract_gdshader(content: &str) -> Vec<SymbolEntry> {
    let orig: Vec<&str> = content.lines().collect();
    let clean = sanitize(content, Lang::GdShader);
    let mut out = Vec::new();
    let mut i = 0;
    while i < clean.len() {
        let t = clean[i].trim();
        if t.is_empty() || t.starts_with('#') {
            i += 1;
            continue;
        }
        let end = find_end(&clean, i, false);
        if let Some(kind) = gdshader_kind(t) {
            let o = orig.get(i).copied().unwrap_or("");
            out.push(SymbolEntry {
                kind: kind.to_string(),
                signature: signature_of(&clean[i], o),
                start_line: i + 1,
                end_line: end + 1,
            });
        }
        i = end + 1;
    }
    out
}

fn gdshader_kind(t: &str) -> Option<&'static str> {
    let (word, rest) = take_word(t);
    match word {
        "shader_type" => return Some("shader_type"),
        "render_mode" => return Some("render_mode"),
        "uniform" => return Some("uniform"),
        "global" | "instance" if rest.trim_start().starts_with("uniform") => {
            return Some("uniform")
        }
        "struct" => return Some("struct"),
        _ => {}
    }
    // Function: `<qualifiers/ret_type> <name>(...)`, all identifier tokens before `(`.
    let (head, _) = t.split_once('(')?;
    let toks: Vec<&str> = head.split_whitespace().collect();
    let all_idents = toks
        .iter()
        .all(|tok| !tok.is_empty() && tok.chars().all(|c| c.is_alphanumeric() || c == '_'));
    (toks.len() >= 2 && all_idents && !matches!(toks[0], "return" | "else")).then_some("fn")
}

/// Value of a `key="value"` attribute inside a Godot section header.
fn tscn_attr(sig: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let start = sig.find(&pat)? + pat.len();
    let len = sig[start..].find('"')?;
    Some(sig[start..start + len].to_string())
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
    match e.kind.as_str() {
        "node" => return tscn_attr(&e.signature, "name").unwrap_or_default(),
        "ext_resource" => return tscn_attr(&e.signature, "path").unwrap_or_default(),
        "connection" => return tscn_attr(&e.signature, "method").unwrap_or_default(),
        "uniform" => {
            let head = e.signature.split([':', '=']).next().unwrap_or("");
            return head
                .split_whitespace()
                .last()
                .map(|t| take_word(t).0.to_string())
                .unwrap_or_default();
        }
        _ => {}
    }
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
        let name = toks
            .next()
            .map(|t| take_word(t).0.to_string())
            .unwrap_or_default();
        if !name.is_empty() {
            return name;
        }
    }
    // Fallback: last word before `(` (and before `=` / `:` for constants).
    let cut: &[char] = if e.kind == "const" {
        &['(', '=', ':']
    } else {
        &['(']
    };
    let sig = e.signature.split(cut).next().unwrap_or("");
    sig.split_whitespace()
        .last()
        .map(|t| take_word(t).0.to_string())
        .unwrap_or_default()
}

fn render_summary(path: &Path, content: &str) -> String {
    let entries = extract_outline(path, content);
    let tops: Vec<String> = entries
        .iter()
        .filter(|e| {
            !matches!(
                e.kind.as_str(),
                "fn" | "method" | "def" | "function" | "func"
            )
        })
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

    fn kinds(path: &str, src: &str) -> Vec<(String, String, usize, usize)> {
        extract_outline(Path::new(path), src)
            .into_iter()
            .map(|e| (e.kind, e.signature, e.start_line, e.end_line))
            .collect()
    }

    #[test]
    fn gdscript_items_and_ranges() {
        let src = r#"class_name Player
extends CharacterBody2D

signal health_changed(new_val)
const MAX_HP = 100
@export var speed: float = 200.0
@onready var sprite: Sprite2D = $Sprite2D

enum State { IDLE, RUN }
enum Dir {
    LEFT,
    RIGHT,
}

func _physics_process(delta: float) -> void:
    var local := 1
    # comment

    move_and_slide()

static func make(a: int,
        b: int) -> Player:
    return null

class Inventory:
    var items := []

    func add(item) -> void: items.append(item)
"#;
        let r = kinds("player.gd", src);
        let has = |k: &str, sig: &str, s: usize, e: usize| {
            assert!(
                r.contains(&(k.into(), sig.into(), s, e)),
                "missing {k} {sig} {s}-{e} in {r:?}"
            );
        };
        has("class_name", "class_name Player", 1, 1);
        has("extends", "extends CharacterBody2D", 2, 2);
        has("signal", "signal health_changed(new_val)", 4, 4);
        has("const", "const MAX_HP = 100", 5, 5);
        has("var", "@export var speed: float = 200.0", 6, 6);
        has("var", "@onready var sprite: Sprite2D = $Sprite2D", 7, 7);
        has("enum", "enum State", 9, 9);
        has("enum", "enum Dir", 10, 13);
        has(
            "func",
            "func _physics_process(delta: float) -> void",
            15,
            19,
        );
        has("func", "static func make(a: int,", 21, 23);
        has("class", "class Inventory", 25, 28);
        has("func", "func add(item) -> void", 28, 28);
        assert!(!r.iter().any(|(_, sig, _, _)| sig.contains("local")));
        let summary = render_summary(Path::new("player.gd"), src);
        assert!(summary.contains("class_name Player"));
        assert!(summary.contains("var speed"));
    }

    #[test]
    fn gdscript_strips_long_initializer() {
        let src = "@export var colors: Array[Color] = [Color.RED, Color.GREEN, Color.BLUE, Color.WHITE]\n";
        assert_eq!(
            kinds("a.gd", src),
            vec![(
                "var".into(),
                "@export var colors: Array[Color]".into(),
                1,
                1
            )]
        );
    }

    #[test]
    fn tscn_sections_and_ranges() {
        let src = r#"[gd_scene load_steps=3 format=3 uid="uid://abc"]

[ext_resource type="Script" path="res://player.gd" id="1"]
[ext_resource type="Texture2D" path="res://icon.svg" id="2"]

[sub_resource type="RectangleShape2D" id="Sub1"]
size = Vector2(10, 10)

[node name="Player" type="CharacterBody2D"]
script = ExtResource("1")

[node name="Sprite2D" type="Sprite2D" parent="."]
texture = ExtResource("2")
position = Vector2(1, 2)

[connection signal="body_entered" from="." to="." method="_on_body_entered"]
"#;
        let r = kinds("main.tscn", src);
        assert_eq!(
            r,
            vec![
                (
                    "ext_resource".into(),
                    r#"[ext_resource type="Script" path="res://player.gd" id="1"]"#.into(),
                    3,
                    3
                ),
                (
                    "node".into(),
                    r#"[node name="Player" type="CharacterBody2D"]"#.into(),
                    9,
                    10
                ),
                (
                    "node".into(),
                    r#"[node name="Sprite2D" type="Sprite2D" parent="."]"#.into(),
                    12,
                    14
                ),
                (
                    "connection".into(),
                    r#"[connection signal="body_entered" from="." to="." method="_on_body_entered"]"#
                        .into(),
                    16,
                    16
                ),
            ]
        );
        let rendered = render_file(Path::new("main.tscn"), src);
        assert!(rendered.contains("\n  L12-L14: [node name=\"Sprite2D\""));
        let summary = render_summary(Path::new("main.tscn"), src);
        assert!(summary.contains("ext_resource res://player.gd"));
        assert!(summary.contains("node Player"));
        assert!(summary.contains("connection _on_body_entered"));
    }

    #[test]
    fn gdshader_items_and_ranges() {
        let src = r#"shader_type canvas_item;
render_mode unshaded, blend_add;

// uniform in comment
uniform vec4 tint : source_color = vec4(1.0);
global uniform float g_time;
const float PI = 3.14;

struct Light {
    vec3 color;
};

vec3 shade(vec3 c,
           float k) {
    return c * k;
}

void fragment() {
    COLOR = texture(TEXTURE, UV) * tint;
}
"#;
        let r = kinds("fx.gdshader", src);
        assert_eq!(
            r,
            vec![
                ("shader_type".into(), "shader_type canvas_item".into(), 1, 1),
                (
                    "render_mode".into(),
                    "render_mode unshaded, blend_add".into(),
                    2,
                    2
                ),
                (
                    "uniform".into(),
                    "uniform vec4 tint : source_color = vec4(1.0)".into(),
                    5,
                    5
                ),
                ("uniform".into(), "global uniform float g_time".into(), 6, 6),
                ("struct".into(), "struct Light".into(), 9, 11),
                ("fn".into(), "vec3 shade(vec3 c,".into(), 13, 16),
                ("fn".into(), "void fragment()".into(), 18, 20),
            ]
        );
        let summary = render_summary(Path::new("fx.gdshader"), src);
        assert!(summary.contains("uniform tint"));
        assert!(summary.contains("struct Light"));
    }

    #[test]
    fn godot_extensions_and_skip_dir() {
        assert_eq!(lang_of(Path::new("a.gd")), Some(Lang::Gd));
        assert_eq!(lang_of(Path::new("a.tscn")), Some(Lang::Tscn));
        assert_eq!(lang_of(Path::new("a.gdshader")), Some(Lang::GdShader));
        assert!(SKIP_DIRS.contains(&".godot"));
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
