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

/// The fields of the TypeScript object type that `opener` starts
/// (`type GameInfo = {`), up to the next `}`, as (name without `?`, type,
/// optional). `//` comments are dropped; a field without `:` has an empty
/// type. Empty when `opener` is absent, so callers assert a non-empty result.
#[cfg(test)]
pub(crate) fn ts_fields(source: &str, opener: &str) -> Vec<(String, String, bool)> {
    let Some((body, _)) = source
        .split_once(opener)
        .and_then(|(_, rest)| rest.split_once('}'))
    else {
        return Vec::new();
    };
    let code: Vec<&str> = body
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect();
    code.join("\n")
        .split(';')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(|field| match field.split_once(':') {
            Some((name, ty)) => {
                let name = name.trim();
                (
                    name.trim_end_matches('?').to_owned(),
                    ty.trim().to_owned(),
                    name.ends_with('?'),
                )
            }
            None => (field.to_owned(), String::new(), false),
        })
        .collect()
}

/// Each field of a serialised JSON object with the TypeScript type it maps to:
/// `string`, `number` or `boolean`, else `unsupported`. Empty for a non-object.
#[cfg(test)]
pub(crate) fn json_fields(value: &serde_json::Value) -> Vec<(String, &'static str)> {
    value.as_object().map_or_default(|object| {
        object
            .iter()
            .map(|(key, value)| {
                let ty = match value {
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::Bool(_) => "boolean",
                    serde_json::Value::Null
                    | serde_json::Value::Array(_)
                    | serde_json::Value::Object(_) => "unsupported",
                };
                (key.clone(), ty)
            })
            .collect()
    })
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

    /// A JSON config file of the frontend package, with its whole-line `//`
    /// comments dropped (the tsconfig files hold no other kind).
    fn read_json(root: &Path, name: &str) -> Result<serde_json::Value, String> {
        let text = std::fs::read_to_string(root.join(name))
            .map_err(|err| format!("cannot read {name}: {err}"))?;
        let kept: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        serde_json::from_str(&kept.join("\n"))
            .map_err(|err| format!("{name} does not parse: {err}"))
    }

    /// The items of a JSON array of strings; `None` for anything else.
    fn strings(value: Option<&serde_json::Value>) -> Option<Vec<&str>> {
        value?
            .as_array()?
            .iter()
            .map(serde_json::Value::as_str)
            .collect()
    }

    /// The Node program's problems, and its `include` globs outside the
    /// package root.
    fn node_program_globs(root: &Path) -> (Vec<String>, Vec<String>) {
        let node = match read_json(root, "tsconfig.node.json") {
            Ok(node) => node,
            Err(err) => return (vec![err], Vec::new()),
        };
        let mut problems = Vec::new();
        let include = strings(node.get("include")).unwrap_or_default();
        if include.is_empty() {
            problems.push(
                "tsconfig.node.json: include is missing, empty or not a list of strings".to_owned(),
            );
        }
        if strings(node.get("exclude")).is_none_or(|exclude| !exclude.is_empty()) {
            problems.push("tsconfig.node.json: exclude is not []".to_owned());
        }
        for key in ["files", "references"] {
            if node.get(key).is_some() {
                problems.push(format!("tsconfig.node.json: has a {key} key"));
            }
        }
        let (at_root, others): (Vec<&str>, Vec<&str>) =
            include.iter().partition(|glob| !glob.contains('/'));
        println!("tsconfig.node.json include: package root {at_root:?}, others {others:?}");
        if at_root != ["*.js", "*.ts"] {
            problems.push(format!(
                "tsconfig.node.json: the package-root globs are {at_root:?}, not [\"*.js\", \"*.ts\"]"
            ));
        }
        (problems, others.into_iter().map(str::to_owned).collect())
    }

    /// eslint's lists of the same files: the package-root globs in
    /// `allowDefaultProject` and the Node-globals block, the others in the
    /// block that lints them with the Node program.
    fn eslint_lists(root: &Path, others: &[String]) -> Vec<String> {
        let text = match std::fs::read_to_string(root.join("eslint.config.js")) {
            Ok(text) => text,
            Err(err) => return vec![format!("cannot read eslint.config.js: {err}")],
        };
        let root_list = "['*.js', '*.ts']";
        let quoted: Vec<String> = others.iter().map(|glob| format!("'{glob}'")).collect();
        let other_list = format!("[{}]", quoted.join(", "));
        let root_count = text.matches(root_list).count();
        let other_count = text.matches(&other_list).count();
        println!(
            "eslint.config.js: {root_list} {root_count} times, {other_list} {other_count} times"
        );
        let mut problems = Vec::new();
        if root_count != 2 {
            problems.push(format!(
                "eslint.config.js holds {root_list} {root_count} times, not 2 (allowDefaultProject and the Node-globals block)"
            ));
        }
        if others.is_empty() || other_count != 1 {
            problems.push(format!(
                "eslint.config.js holds {other_list} {other_count} times, not once"
            ));
        }
        problems
    }

    /// The app program leaves the tests to the Node program and never loads
    /// Node's types.
    fn app_program(root: &Path) -> Vec<String> {
        let app = match read_json(root, "tsconfig.json") {
            Ok(app) => app,
            Err(err) => return vec![err],
        };
        let mut problems = Vec::new();
        let types = app
            .get("compilerOptions")
            .and_then(|options| options.get("types"));
        if types
            .is_some_and(|types| strings(Some(types)).is_none_or(|types| types.contains(&"node")))
        {
            problems.push("tsconfig.json: compilerOptions.types names node".to_owned());
        }
        let exclude = strings(app.get("exclude"));
        println!("tsconfig.json exclude: {exclude:?}");
        if exclude != Some(vec!["src/**/*.test.ts"]) {
            problems.push(format!(
                "tsconfig.json: exclude is {exclude:?}, not [\"src/**/*.test.ts\"]"
            ));
        }
        problems
    }

    /// knip's entries are exactly the gate helpers under `scripts/`, each
    /// with the production mark.
    fn knip_entries(root: &Path) -> Vec<String> {
        let knip = match read_json(root, "knip.json") {
            Ok(knip) => knip,
            Err(err) => return vec![err],
        };
        let mut problems = Vec::new();
        let keys: Vec<&str> = knip
            .as_object()
            .map_or_default(|object| object.keys().map(String::as_str).collect());
        if keys != ["entry"] {
            problems.push(format!("knip.json: its keys are {keys:?}, not [\"entry\"]"));
        }
        let entries = strings(knip.get("entry")).unwrap_or_default();
        let mut named = std::collections::BTreeSet::new();
        for entry in &entries {
            let Some(file) = entry.strip_suffix('!') else {
                problems.push(format!("knip.json: {entry} lacks the production mark !"));
                continue;
            };
            if !root.join(file).is_file() {
                problems.push(format!("knip.json: {entry} names no file"));
            }
            named.insert(file.to_owned());
        }
        let helpers: std::collections::BTreeSet<String> =
            match script_files(root, Path::new("scripts")) {
                Ok(files) => files
                    .iter()
                    .map(|file| file.to_string_lossy().replace('\\', "/"))
                    .collect(),
                Err(err) => {
                    problems.push(err);
                    std::collections::BTreeSet::new()
                }
            };
        println!(
            "knip entries {}, gate helper files {}",
            entries.len(),
            helpers.len()
        );
        if entries.is_empty() || helpers.is_empty() {
            problems.push("knip.json or scripts/ lists no gate helper".to_owned());
        }
        let unlisted: Vec<&String> = helpers.difference(&named).collect();
        let extra: Vec<&String> = named.difference(&helpers).collect();
        if !unlisted.is_empty() || !extra.is_empty() {
            problems.push(format!(
                "knip.json lacks an entry for the gate helpers {unlisted:?} and names {extra:?}, which are not gate helpers"
            ));
        }
        problems
    }

    #[test]
    fn node_files_are_listed_once() {
        let root = count_in_frontend(&[]).root;
        let (mut failures, others) = node_program_globs(&root);
        failures.extend(eslint_lists(&root, &others));
        failures.extend(app_program(&root));
        failures.extend(knip_entries(&root));
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
