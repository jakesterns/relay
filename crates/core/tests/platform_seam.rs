//! The portability rule, enforced: every use of the `windows` crate lives in a
//! platform module.
//!
//! A *platform module* is one whose compilation is gated on Windows — a file
//! with `#![cfg(windows)]`, a file whose `mod` declaration carries
//! `#[cfg(windows)]` (or sits inside such a module), or an inline
//! `#[cfg(windows)] mod … { }` block. A `#[cfg(windows)]` on a lone function,
//! field or statement does not count: that is how Windows calls leak into
//! code that is meant to be portable, and how a stub ends up silently
//! returning a default instead of saying the capability is unsupported.
//!
//! Scans `crates/` and `ui/src-tauri/` as text. It relies on rustfmt layout
//! (an inline module's closing brace sits at the indentation of its `mod`
//! line), which `cargo fmt --check` already guarantees. Runs on every target,
//! so a macOS or Linux CI job enforces it too. See `docs/dev/porting.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if !matches!(name.as_ref(), "target" | "node_modules" | "gen" | "fixtures") {
                rust_files(&p, out);
            }
        } else if name.ends_with(".rs") {
            out.push(p);
        }
    }
}

/// `cfg(...)` predicate that only holds on Windows.
fn is_windows_cfg(attr: &str) -> bool {
    let a: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
    if !(a.starts_with("#[cfg(") || a.starts_with("#![cfg(")) || a.contains("not(windows)") {
        return false;
    }
    let pred = &a[a.find("cfg(").unwrap() + 4..];
    // `all(windows, …)` and bare `windows` gate; `any(windows, …)` does not.
    pred.starts_with("windows)")
        || pred.starts_with("target_os=\"windows\")")
        || (pred.starts_with("all(")
            && (pred.contains("(windows,")
                || pred.contains(",windows)")
                || pred.contains(",windows,")
                || pred.contains("target_os=\"windows\"")))
}

struct Decl {
    child: PathBuf,
    gated: bool,
}

struct Scan {
    file_gated: bool,
    decls: Vec<Decl>,
    /// Lines that name the `windows` crate outside a gated inline module.
    ungated_uses: Vec<(usize, String)>,
}

fn mod_line(line: &str) -> Option<(String, bool)> {
    let t = line.trim_start();
    let t = t.strip_prefix("pub ").or_else(|| t.strip_prefix("pub(crate) ")).unwrap_or(t);
    let rest = t.strip_prefix("mod ")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let after = rest[name.len()..].trim();
    match after {
        ";" => Some((name, false)),
        "{" => Some((name, true)),
        _ => None,
    }
}

fn scan(path: &Path) -> Scan {
    let text = std::fs::read_to_string(path).unwrap();
    let dir = path.parent().unwrap();
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let is_dir_owner = matches!(stem.as_str(), "mod" | "lib" | "main" | "build")
        || path.parent().is_some_and(|p| p.ends_with("bin") || p.ends_with("tests"));
    let child_dir = if is_dir_owner { dir.to_path_buf() } else { dir.join(&stem) };

    let mut out = Scan { file_gated: false, decls: Vec::new(), ungated_uses: Vec::new() };
    let mut pending: Vec<String> = Vec::new();
    // Indentation of each open inline module and whether it is gated.
    let mut open: Vec<(usize, bool)> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if let Some(&(ind, _)) = open.last() {
            if indent == ind && line.starts_with('}') {
                open.pop();
                pending.clear();
                continue;
            }
        }
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        if line.starts_with("#![") {
            if is_windows_cfg(line) {
                out.file_gated = true;
            }
            continue;
        }
        if line.starts_with("#[") {
            pending.push(line.to_string());
            continue;
        }
        let in_gated = open.iter().any(|&(_, g)| g);
        if let Some((name, inline)) = mod_line(raw) {
            let gated = in_gated || pending.iter().any(|a| is_windows_cfg(a));
            if inline {
                open.push((indent, gated));
            } else {
                let explicit = pending.iter().find_map(|a| {
                    let a = a.strip_prefix("#[path")?.trim_start().strip_prefix('=')?;
                    Some(a.trim().trim_end_matches(']').trim().trim_matches('"').to_string())
                });
                let child = match explicit {
                    Some(p) => dir.join(p),
                    None if child_dir.join(format!("{name}.rs")).exists() => {
                        child_dir.join(format!("{name}.rs"))
                    }
                    None => child_dir.join(&name).join("mod.rs"),
                };
                out.decls.push(Decl { child, gated });
            }
            pending.clear();
            continue;
        }
        pending.clear();
        let code = strip_strings(line.split("//").next().unwrap_or(""));
        if !in_gated && (code.contains("windows::") || code.contains("windows_core::")) {
            out.ungated_uses.push((i + 1, line.to_string()));
        }
    }
    out
}

/// Drops the contents of `"..."` literals so a message naming the crate is not
/// a use of it.
fn strip_strings(code: &str) -> String {
    let mut out = String::new();
    let mut in_str = false;
    let mut escaped = false;
    for c in code.chars() {
        if in_str {
            match (escaped, c) {
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => escaped = false,
            }
        } else if c == '"' {
            in_str = true;
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn windows_api_use_is_confined_to_platform_modules() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("ui/src-tauri"), &mut files);
    assert!(files.len() > 100, "scan found only {} files under {}", files.len(), root.display());

    let scans: BTreeMap<PathBuf, Scan> =
        files.iter().map(|f| (f.canonicalize().unwrap(), scan(f))).collect();

    // Module tree edges: child -> (parent, declaration gated).
    let mut parent: BTreeMap<PathBuf, (PathBuf, bool)> = BTreeMap::new();
    for (file, s) in &scans {
        for d in &s.decls {
            if let Ok(c) = d.child.canonicalize() {
                parent.insert(c, (file.clone(), d.gated));
            }
        }
    }
    fn gated(
        f: &Path,
        scans: &BTreeMap<PathBuf, Scan>,
        parent: &BTreeMap<PathBuf, (PathBuf, bool)>,
        seen: &mut BTreeSet<PathBuf>,
    ) -> bool {
        if !seen.insert(f.to_path_buf()) {
            return false;
        }
        if scans.get(f).is_some_and(|s| s.file_gated) {
            return true;
        }
        match parent.get(f) {
            Some((p, decl_gated)) => *decl_gated || gated(p, scans, parent, seen),
            None => false,
        }
    }

    let mut violations = Vec::new();
    for (file, s) in &scans {
        if s.ungated_uses.is_empty() || gated(file, &scans, &parent, &mut BTreeSet::new()) {
            continue;
        }
        let rel = file.strip_prefix(&root).unwrap_or(file).display().to_string();
        for (line, text) in &s.ungated_uses {
            violations.push(format!("{rel}:{line}: {text}"));
        }
    }
    assert!(
        violations.is_empty(),
        "{} use(s) of the windows crate outside a #[cfg(windows)] module \
         (move them into a platform module; see docs/dev/porting.md):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

#[test]
fn cfg_predicates_are_classified() {
    assert!(is_windows_cfg("#[cfg(windows)]"));
    assert!(is_windows_cfg("#![cfg(windows)]"));
    assert!(is_windows_cfg("#[cfg(all(windows, feature = \"com\"))]"));
    assert!(is_windows_cfg("#[cfg(all(test, windows))]"));
    assert!(is_windows_cfg("#[cfg(target_os = \"windows\")]"));
    assert!(!is_windows_cfg("#[cfg(not(windows))]"));
    assert!(!is_windows_cfg("#[cfg(any(windows, test))]"));
    assert!(!is_windows_cfg("#[cfg(test)]"));
    assert!(!is_windows_cfg("#[derive(Debug)]"));
}
