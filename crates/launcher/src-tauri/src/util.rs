//! Helpers shared across modules.

use std::fmt::Display;
#[cfg(test)]
use std::path::PathBuf;

/// Log a fire-and-forget failure instead of discarding it.
///
/// For calls whose failure changes nothing for the caller -- a window that will
/// not focus, an event with no listener -- but that must not vanish silently:
/// `let _ = ...` hides exactly the failures worth reading in a bug report.
pub(crate) fn log_if_err<T, E: Display>(what: &str, result: Result<T, E>) {
    if let Err(err) = result {
        tracing::warn!("{what} failed: {err}");
    }
}

/// Walk every `.rs` file under this crate's `src/`, skipping any file named
/// `skip_file`, and return (files scanned, occurrences of `needle`).
///
/// For tests that pin where a call may appear. Callers assert on both numbers,
/// so a scan that found no files fails instead of passing vacuously, and build
/// their needle so their own source does not contain it contiguously.
#[cfg(test)]
pub(crate) fn count_in_sources(needle: &str, skip_file: Option<&str>) -> (usize, usize) {
    let mut dirs = vec![sources_root()];
    let (mut files, mut hits) = (0, 0);
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let name = path.file_name().and_then(|name| name.to_str());
            if path.extension().is_none_or(|ext| ext != "rs")
                || (skip_file.is_some() && name == skip_file)
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                files += 1;
                hits += text.matches(needle).count();
            }
        }
    }
    (files, hits)
}

/// This crate's `src/`.
#[cfg(test)]
fn sources_root() -> PathBuf {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    // A Windows test binary cross-compiled from WSL carries a Linux path here,
    // which Windows cannot open. Cargo runs tests from the package root, so the
    // relative path names the same directory.
    if root.is_dir() {
        root
    } else {
        PathBuf::from("src")
    }
}

/// Per file under this crate's `src/`, the occurrences of `needle` outside
/// `#[cfg(test)]` inline modules.
///
/// For checks that pin a call to one file in shipped code: a test's fixtures
/// never count. One entry per `.rs` file, zero counts included, path relative
/// to `src/`, sorted. A `#[cfg(test)]` item that is not a module is counted.
#[cfg(test)]
pub(crate) fn count_in_sources_by_file(needle: &str) -> Vec<(PathBuf, usize)> {
    let root = sources_root();
    let mut dirs = vec![PathBuf::new()];
    let mut counts = Vec::new();
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(root.join(&dir))
            .unwrap_or_else(|err| panic!("cannot list {}: {err}", root.join(&dir).display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|err| panic!("cannot list {}: {err}", dir.display()));
            let file = dir.join(entry.file_name());
            if root.join(&file).is_dir() {
                dirs.push(file);
            } else if file.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(root.join(&file))
                    .unwrap_or_else(|err| panic!("cannot read {}: {err}", file.display()));
                let count = strip_test_modules(&text).matches(needle).count();
                counts.push((file, count));
            }
        }
    }
    assert!(
        !counts.is_empty(),
        "no .rs file found under {}",
        root.display()
    );
    counts.sort();
    counts
}

/// What `count_in_frontend` read and found.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FrontendScan {
    /// The directory `files` are relative to: the web frontend's package root.
    pub(crate) root: PathBuf,
    /// Every file read, relative to `root` (`src/main.ts`, `index.html`), sorted.
    pub(crate) files: Vec<PathBuf>,
    /// Per needle, in the order given: each occurrence as (file, 1-based line).
    pub(crate) hits: Vec<Vec<(PathBuf, usize)>>,
}

/// Read every `.svelte`, `.ts` and `.js` file under the web frontend's `src/`
/// (test files included) plus `index.html`, and find each needle in them.
///
/// The one walker every frontend scan uses. A needle that starts with an
/// identifier character counts only where the character before it is not one
/// (`eval(` does not match `retrieval(`).
#[cfg(test)]
pub(crate) fn count_in_frontend(needles: &[&str]) -> FrontendScan {
    let mut root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
    // Same reason as `sources_root`: tests run from the package root.
    if !root.join("src").is_dir() {
        root = PathBuf::from("..");
    }
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::from("src")];
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(root.join(&dir))
            .unwrap_or_else(|err| panic!("cannot list {}: {err}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|err| panic!("cannot list {}: {err}", dir.display()));
            let file = dir.join(entry.file_name());
            if root.join(&file).is_dir() {
                dirs.push(file);
            } else if file
                .extension()
                .is_some_and(|ext| ext == "svelte" || ext == "ts" || ext == "js")
            {
                files.push(file);
            }
        }
    }
    assert!(
        !files.is_empty(),
        "no frontend source found under {}",
        root.join("src").display()
    );
    files.push(PathBuf::from("index.html"));
    files.sort();
    let mut hits = vec![Vec::new(); needles.len()];
    for file in &files {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", file.display()));
        for (needle, found) in needles.iter().zip(&mut hits) {
            found.extend(
                needle_lines(&text, needle)
                    .into_iter()
                    .map(|line| (file.clone(), line)),
            );
        }
    }
    FrontendScan { root, files, hits }
}

/// The 1-based line of each occurrence of `needle` in `text`, one entry per
/// occurrence. A needle starting with `[A-Za-z0-9_$]` counts only where the
/// byte before it is not in that set.
#[cfg(test)]
fn needle_lines(text: &str, needle: &str) -> Vec<usize> {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$';
    let bounded = needle.as_bytes().first().copied().is_some_and(is_word);
    let bytes = text.as_bytes();
    let (mut line, mut scanned) = (1, 0);
    let mut lines = Vec::new();
    for (at, _) in text.match_indices(needle) {
        if bounded
            && at
                .checked_sub(1)
                .and_then(|before| bytes.get(before))
                .copied()
                .is_some_and(is_word)
        {
            continue;
        }
        line += text
            .get(scanned..at)
            .map_or(0, |gap| gap.matches('\n').count());
        scanned = at;
        lines.push(line);
    }
    lines
}

/// `src` without its `#[cfg(test)]` inline modules.
///
/// A small lexer tracks comments, strings, raw strings and char literals, so a
/// brace or an attribute inside one neither starts nor ends a module. Only in
/// code does `#[cfg(test)]`, then optional further attributes and visibility,
/// then `mod <name> {` start a cut, which ends at the matching `}`.
#[cfg(test)]
fn strip_test_modules(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut kept = String::with_capacity(src.len());
    let (mut at, mut depth) = (0, 0_usize);
    while at < chars.len() {
        if depth == 0 {
            if let Some(body) = test_module_body(&chars, at) {
                depth = 1;
                at = body;
                continue;
            }
        }
        let end = token_end(&chars, at);
        if depth == 0 {
            kept.extend(&chars[at..end]);
        } else if end == at + 1 {
            match chars[at] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        at = end;
    }
    kept
}

/// Where the token starting at `at` ends: a comment, a string, a raw string, a
/// char literal, or else a single character (a lifetime's `'` included).
#[cfg(test)]
fn token_end(chars: &[char], at: usize) -> usize {
    let get = |i: usize| chars.get(i).copied();
    let len = chars.len();
    match (get(at), get(at + 1)) {
        (Some('/'), Some('/')) => (at..len).find(|&i| chars[i] == '\n').unwrap_or(len),
        (Some('/'), Some('*')) => {
            let (mut i, mut depth) = (at, 0_usize);
            while i < len {
                match (chars[i], get(i + 1)) {
                    ('/', Some('*')) => {
                        depth += 1;
                        i += 2;
                    }
                    ('*', Some('/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            return i;
                        }
                    }
                    _ => i += 1,
                }
            }
            len
        }
        (Some('"'), _) => {
            let mut i = at + 1;
            while i < len {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => return i + 1,
                    _ => i += 1,
                }
            }
            len
        }
        (Some('r'), Some('#' | '"')) => {
            let hashes = chars[at + 1..].iter().take_while(|&&c| c == '#').count();
            if get(at + 1 + hashes) != Some('"') {
                return at + 1;
            }
            let closes =
                |i: usize| chars[i] == '"' && (1..=hashes).all(|k| get(i + k) == Some('#'));
            (at + 2 + hashes..len)
                .find(|&i| closes(i))
                .map_or(len, |i| i + 1 + hashes)
        }
        (Some('\''), Some('\\')) => (at + 3..len)
            .find(|&i| chars[i] == '\'')
            .map_or(len, |i| i + 1),
        (Some('\''), Some(_)) if get(at + 2) == Some('\'') => at + 3,
        _ => at + 1,
    }
}

/// If `#[cfg(test)]` at `at` opens an inline module, the index just past its `{`.
#[cfg(test)]
fn test_module_body(chars: &[char], at: usize) -> Option<usize> {
    const CFG_TEST: &str = "#[cfg(test)]";
    if !starts_with(chars, at, CFG_TEST) {
        return None;
    }
    let mut i = skip_trivia(chars, at + CFG_TEST.len());
    while starts_with(chars, i, "#[") {
        i = skip_trivia(chars, attribute_end(chars, i)?);
    }
    if let Some(vis) = ["pub(crate)", "pub(super)", "pub"]
        .into_iter()
        .find(|vis| starts_with(chars, i, vis))
    {
        i = skip_trivia(chars, i + vis.len());
    }
    if !starts_with(chars, i, "mod") {
        return None;
    }
    let name = skip_trivia(chars, i + 3);
    let name_end = (name..chars.len())
        .find(|&j| !(chars[j].is_alphanumeric() || chars[j] == '_'))
        .unwrap_or(chars.len());
    if name == i + 3 || name_end == name {
        return None;
    }
    let brace = skip_trivia(chars, name_end);
    (chars.get(brace) == Some(&'{')).then_some(brace + 1)
}

/// The index just past the `]` closing the attribute that starts at `at`.
#[cfg(test)]
fn attribute_end(chars: &[char], at: usize) -> Option<usize> {
    let (mut i, mut depth) = (at + 1, 0_usize);
    while i < chars.len() {
        let end = token_end(chars, i);
        if end == i + 1 {
            match chars[i] {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(end);
                    }
                }
                _ => {}
            }
        }
        i = end;
    }
    None
}

/// Past any whitespace and comments from `at`.
#[cfg(test)]
fn skip_trivia(chars: &[char], mut at: usize) -> usize {
    loop {
        if chars.get(at).is_some_and(|c| c.is_whitespace()) {
            at += 1;
        } else if starts_with(chars, at, "//") || starts_with(chars, at, "/*") {
            at = token_end(chars, at);
        } else {
            return at;
        }
    }
}

#[cfg(test)]
fn starts_with(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(k, c)| chars.get(at + k) == Some(&c))
}

#[cfg(test)]
mod tests {
    use super::{
        count_in_frontend, count_in_sources, count_in_sources_by_file, needle_lines,
        strip_test_modules,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn needle_boundaries() {
        let lines = needle_lines(
            "eval(x)\nretrieval(y)\n$eval(z)\na.eval(w) eval(v)",
            "eval(",
        );
        println!("identifier needle: {lines:?}");
        assert_eq!(lines, [1, 4, 4]);
        let lines = needle_lines("x'open_url'", "'open_url'");
        println!("quoted needle: {lines:?}");
        assert_eq!(lines, [1]);
    }

    #[test]
    fn test_modules_are_stripped() {
        let fixture = r##"
fn a() { NEEDLE(); }
#[cfg(test)]
mod tests {
    const S: &str = "}";
    const C: char = '{';
    const R: &str = r#"}"#;
    // }
    /* } /* nested } */ */
    fn f<'a>(x: &'a str) -> &'a str { x }
    mod inner { fn g() {} }
    #[test] fn t() { NEEDLE(); }
}
fn b() { NEEDLE(); }
#[cfg(test)]
pub(crate) fn helper() { NEEDLE(); }
"##;
        let kept = strip_test_modules(fixture);
        println!("kept:\n{kept}");
        assert_eq!(kept.matches("NEEDLE").count(), 3);

        let attributed =
            "#[cfg(test)]\n#[allow(dead_code)]\npub(crate) mod t { fn x() { NEEDLE(); } }";
        let kept = strip_test_modules(attributed);
        println!("kept: {kept:?}");
        assert_eq!(kept.matches("NEEDLE").count(), 0);
    }

    #[test]
    fn per_file_scan_skips_test_modules() {
        let needle = concat!("fn hotkey_labels_are", "_the_documented_chords");
        let (files, whole) = count_in_sources(needle, None);
        let per_file = count_in_sources_by_file(needle);
        let outside: usize = per_file.iter().map(|(_, count)| count).sum();
        println!("{files} files scanned; {needle}: {whole} in whole files, {outside} outside test modules");
        assert_eq!(per_file.len(), files);
        assert!(whole >= 1);
        assert_eq!(outside, 0);

        let needle = concat!("fn show_main", "_window(");
        let per_file = count_in_sources_by_file(needle);
        let outside: usize = per_file.iter().map(|(_, count)| count).sum();
        let found: Vec<(PathBuf, usize)> = per_file
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .collect();
        println!("{needle}: {outside} outside test modules, in {found:?}");
        assert_eq!(found, [(PathBuf::from("main.rs"), 1)]);
    }

    #[test]
    fn frontend_scan_walks_the_app() {
        let scan = count_in_frontend(&[concat!("<div id=", "\"app\">")]);
        for kind in ["svelte", "ts", "js", "html"] {
            let count = scan
                .files
                .iter()
                .filter(|file| file.extension().is_some_and(|ext| ext == kind))
                .count();
            println!("{kind}: {count} files");
        }
        assert!(scan
            .files
            .contains(&PathBuf::from("src/lib/components/Overlay.svelte")));
        assert!(scan.files.contains(&PathBuf::from("index.html")));
        for file in &scan.files {
            let source = file.starts_with("src")
                && file
                    .extension()
                    .is_some_and(|ext| ext == "svelte" || ext == "ts" || ext == "js");
            assert!(
                source || file == Path::new("index.html"),
                "unexpected file {}",
                file.display()
            );
            assert!(
                scan.root.join(file).is_file(),
                "{} is not under the scan root",
                file.display()
            );
        }
        println!("hits: {:?}", scan.hits[0]);
        assert_eq!(scan.hits[0], [(PathBuf::from("index.html"), 9)]);
    }

    /// For one source line: `None` when it holds no eslint-disable or
    /// svelte-ignore directive, else whether the directive gives a reason --
    /// ` -- <reason>` for eslint, a `(<reason>)` note after the codes for
    /// svelte-ignore (eslint-plugin-svelte reads a ` -- ` there as more codes).
    fn directive_reason(line: &str) -> Option<bool> {
        for (open, close) in [("//", "\n"), ("/*", "*/"), ("<!--", "-->")] {
            let mut rest = line;
            while let Some((_, after)) = rest.split_once(open) {
                rest = after;
                let comment = after.split_once(close).map_or(after, |(inside, _)| inside);
                let comment = comment.trim_start();
                let (body, svelte) = if let Some(body) = comment.strip_prefix("svelte-ignore") {
                    (body, true)
                } else if let Some(body) = [
                    "eslint-disable-next-line",
                    "eslint-disable-line",
                    "eslint-disable",
                ]
                .iter()
                .find_map(|keyword| comment.strip_prefix(keyword))
                {
                    (body, false)
                } else {
                    continue;
                };
                if !(body.is_empty() || body.starts_with(char::is_whitespace)) {
                    continue;
                }
                let reason = if svelte {
                    body.split_once('(')
                        .and_then(|(_, note)| note.split_once(')'))
                        .map(|(note, _)| note)
                } else {
                    body.split_once(" -- ").map(|(_, reason)| reason)
                };
                return Some(reason.is_some_and(|reason| !reason.trim().is_empty()));
            }
        }
        None
    }

    /// The `.js`, `.ts` and `.mjs` files directly in `root.join(dir)`, as paths
    /// relative to `root`. A directory that cannot be listed is an error, never
    /// an empty list.
    fn script_files(root: &Path, dir: &Path) -> Result<Vec<PathBuf>, String> {
        let listing =
            |err: std::io::Error| format!("cannot list {}: {err}", root.join(dir).display());
        let mut files = Vec::new();
        for entry in std::fs::read_dir(root.join(dir)).map_err(listing)? {
            let file = dir.join(entry.map_err(listing)?.file_name());
            let script = file
                .extension()
                .is_some_and(|ext| ext == "js" || ext == "ts" || ext == "mjs");
            if script && root.join(&file).is_file() {
                files.push(file);
            }
        }
        Ok(files)
    }

    /// Lines with their expected `directive_reason`, written out literally.
    const PLANTED_DIRECTIVES: [(&str, Option<bool>); 14] = [
        ("const s = 'eslint-disable';", None),
        ("// see https://example.invalid/x -- not a directive", None),
        ("// eslint-disabled rules are listed below", None),
        (
            "// eslint-disable-next-line svelte/prefer-svelte-reactivity",
            Some(false),
        ),
        ("// eslint-disable-line no-console --", Some(false)),
        ("/* eslint-disable no-alert -- */", Some(false)),
        ("/* eslint-disable */", Some(false)),
        ("<!-- svelte-ignore a11y_autofocus -->", Some(false)),
        (
            "<!-- svelte-ignore a11y_autofocus -- focus moves here on open -->",
            Some(false),
        ),
        ("<!-- svelte-ignore a11y_autofocus () -->", Some(false)),
        (
            "// eslint-disable-next-line no-console -- the one line a script prints",
            Some(true),
        ),
        (
            "/* eslint-disable no-alert -- a native confirm is the design */",
            Some(true),
        ),
        (
            "<!-- svelte-ignore a11y_autofocus (focus moves here when the dialog opens) -->",
            Some(true),
        ),
        (
            "let a = 1; // eslint-disable-line prefer-const -- set once, read by the template",
            Some(true),
        ),
    ];

    #[test]
    fn frontend_suppressions_give_a_reason() {
        let mut failures = Vec::new();
        for (line, expected) in PLANTED_DIRECTIVES {
            let verdict = directive_reason(line);
            println!("planted: {line} -> {verdict:?}");
            if verdict != expected {
                failures.push(format!(
                    "planted: {line} -> {verdict:?}, expected {expected:?}"
                ));
            }
        }
        let scan = count_in_frontend(&[]);
        let root = scan.root;
        let mut files = scan.files;
        for dir in [Path::new(""), Path::new("scripts")] {
            match script_files(&root, dir) {
                Ok(found) => files.extend(found),
                Err(err) => failures.push(err),
            }
        }
        files.sort();
        println!("{} files scanned", files.len());
        let mut directives = 0;
        for file in &files {
            let text = match std::fs::read_to_string(root.join(file)) {
                Ok(text) => text,
                Err(err) => {
                    failures.push(format!("cannot read {}: {err}", file.display()));
                    continue;
                }
            };
            for (at, line) in text.lines().enumerate() {
                let Some(reason) = directive_reason(line) else {
                    continue;
                };
                directives += 1;
                let place = format!("{}:{}", file.display(), at + 1);
                println!(
                    "{place}: {}",
                    if reason { "reason given" } else { "NO REASON" }
                );
                if !reason {
                    failures.push(format!(
                        "{place}: `{}` gives no reason (eslint: add \" -- <reason>\"; svelte-ignore: add \"(<reason>)\" after the codes)",
                        line.trim()
                    ));
                }
            }
        }
        println!("{directives} directives");
        assert!(!files.is_empty(), "the scan read no file");
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
