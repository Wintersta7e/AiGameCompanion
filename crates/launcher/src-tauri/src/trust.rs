//! Which pages the app's windows may load, and the checks that pin each
//! window's permissions, page policy and HTML sinks.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Runtime, Url};

/// (scheme, host, port) of each origin tauri serves the bundled pages from.
const APP_ORIGINS: [(&str, &str, Option<u16>); 3] = [
    ("tauri", "localhost", None),
    ("http", "tauri.localhost", Some(80)),
    ("https", "tauri.localhost", Some(443)),
];

/// Whether `url` is one of the app's own pages: an origin tauri serves the
/// bundled frontend from, or `dev_url`'s origin while the dev server is used.
///
/// Compares scheme, host and port rather than `Url::origin()`, which is
/// opaque for `tauri:` URLs and never compares equal.
pub(crate) fn is_app_url(url: &Url, dev_url: Option<&Url>) -> bool {
    let (scheme, host, port) = (url.scheme(), url.host_str(), url.port_or_known_default());
    APP_ORIGINS.iter().any(|&(app_scheme, app_host, app_port)| {
        scheme == app_scheme && host == Some(app_host) && port == app_port
    }) || dev_url.is_some_and(|dev| {
        scheme == dev.scheme()
            && host.is_some()
            && host == dev.host_str()
            && port == dev.port_or_known_default()
    })
}

/// A plugin that keeps every window on the app's own pages. A refused
/// navigation is logged by window and scheme only: the address can carry
/// what the user searched for.
pub(crate) fn navigation_guard<R: Runtime>(dev_url: Option<Url>) -> TauriPlugin<R> {
    Builder::new("navigation-guard")
        .on_navigation(move |webview, url| {
            let allowed = is_app_url(url, dev_url.as_ref());
            if !allowed {
                tracing::warn!(
                    "Navigation refused in the {} window: {} address",
                    webview.label(),
                    url.scheme()
                );
            }
            allowed
        })
        .build()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        reason = "a panic is how a test reports a failed assumption, and the checks print what they compared"
    )]

    use super::is_app_url;
    use crate::util::count_in_frontend;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use tauri::Url;

    #[test]
    fn navigation_allows_only_app_origins() {
        const DEV: Option<&str> = Some("http://localhost:1420");
        let rows: [(&str, Option<&str>, bool); 16] = [
            ("tauri://localhost/", None, true),
            ("http://tauri.localhost/x", None, true),
            ("https://tauri.localhost/", None, true),
            ("tauri://localhost/", DEV, true),
            ("http://localhost:1420/", DEV, true),
            ("http://localhost:1420/", None, false),
            ("http://localhost:1421/", DEV, false),
            ("https://example.com/", None, false),
            ("https://example.com/", DEV, false),
            ("https://tauri.localhost.example.com/", None, false),
            ("javascript:alert(1)", None, false),
            ("data:text/html,x", None, false),
            ("blob:http://tauri.localhost/x", None, false),
            ("file:///C:/x", None, false),
            ("about:blank", None, false),
            ("about:blank", DEV, false),
        ];
        let mut wrong = Vec::new();
        for (url, dev, expected) in rows {
            let parsed = Url::parse(url).unwrap();
            let dev_url = dev.map(|dev| Url::parse(dev).unwrap());
            if is_app_url(&parsed, dev_url.as_ref()) != expected {
                wrong.push(format!("{url} (dev {dev:?}) should be {expected}"));
            }
        }
        println!("{} rows checked", rows.len());
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The one component allowed to insert raw HTML, and only once it exists.
    const SUGGESTION: &str = "src/lib/components/SearchSuggestion.svelte";

    /// Ways to insert raw HTML, run text as code or leave the app page, none of
    /// which may appear anywhere in the web frontend.
    const NEVER_IN_FRONTEND: [&str; 27] = [
        "{@html",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "createContextualFragment",
        "setHTMLUnsafe",
        "parseHTMLUnsafe",
        "DOMParser",
        "srcdoc",
        "eval(",
        "new Function",
        "<a ",
        "<a>",
        "<iframe",
        "<form",
        "<object",
        "<embed",
        "window.open",
        "location.href",
        "location.assign",
        "location.replace",
        "no-at-html-tags",
        "marked.parse",
        "parseInline",
        "new Parser",
        "marked.use",
        "setOptions",
    ];

    #[test]
    fn frontend_has_one_html_sink() {
        const SINKS: [&str; 2] = ["innerHTML", "attachShadow"];
        const MARKED: &str = "from 'marked'";
        const OPEN_URL: &str = "'open_url'";
        let needles: Vec<&str> = SINKS
            .iter()
            .chain(&NEVER_IN_FRONTEND)
            .chain(&["<style", MARKED, OPEN_URL])
            .copied()
            .collect();
        let scan = count_in_frontend(&needles);
        let hits_of = |needle: &str| &scan.hits[needles.iter().position(|n| *n == needle).unwrap()];
        let has = |file: &str| scan.files.iter().any(|scanned| scanned == Path::new(file));

        for kind in ["svelte", "ts", "js", "html"] {
            let count = scan
                .files
                .iter()
                .filter(|file| file.extension().is_some_and(|ext| ext == kind))
                .count();
            println!("{kind}: {count} files");
        }
        println!("{} files scanned", scan.files.len());
        for (needle, hits) in needles.iter().zip(&scan.hits) {
            println!("{needle}: {}", hits.len());
            for (file, line) in hits {
                println!("  {}:{line}", file.display());
            }
        }

        let mut failures = Vec::new();
        let mut required = vec!["src/lib/components/Overlay.svelte"];
        if has("src/lib/components/Markdown.svelte") || has("src/lib/utils/markdown.ts") {
            required.extend([
                "src/lib/components/Markdown.svelte",
                "src/lib/utils/markdown.ts",
            ]);
        }
        for file in required {
            if !has(file) {
                failures.push(format!("{file} was not scanned"));
            }
        }
        let suggestion = has(SUGGESTION);
        for sink in SINKS {
            let hits = hits_of(sink);
            let allowed = if suggestion {
                hits.len() == 1 && hits[0].0 == Path::new(SUGGESTION)
            } else {
                hits.is_empty()
            };
            if !allowed {
                failures.push(format!(
                    "{sink}: {hits:?}, allowed only once in {SUGGESTION}"
                ));
            }
        }
        for needle in NEVER_IN_FRONTEND {
            let hits = hits_of(needle);
            if !hits.is_empty() {
                failures.push(format!("{needle} must not appear: {hits:?}"));
            }
        }
        if include_str!("../../eslint.config.js").contains("no-at-html-tags") {
            failures.push("eslint.config.js turns off or overrides no-at-html-tags".to_owned());
        }
        let styles = hits_of("<style");
        if styles
            .iter()
            .any(|(file, _)| file == Path::new("index.html"))
        {
            failures.push(format!(
                "index.html must carry no <style element: {styles:?}"
            ));
        }
        let importers: BTreeSet<&Path> = hits_of(MARKED)
            .iter()
            .map(|(file, _)| file.as_path())
            .filter(|file| !file.to_string_lossy().ends_with(".test.ts"))
            .collect();
        let expected: BTreeSet<&Path> = if has("src/lib/utils/markdown.ts") {
            BTreeSet::from([Path::new("src/lib/utils/markdown.ts")])
        } else {
            BTreeSet::new()
        };
        if importers != expected {
            failures.push(format!("{MARKED}: {importers:?}, expected {expected:?}"));
        }
        let open_url = hits_of(OPEN_URL);
        if !(open_url.len() == 1 && open_url[0].0 == Path::new("src/lib/utils/links.ts")) {
            failures.push(format!(
                "{OPEN_URL}: {open_url:?}, allowed only once in src/lib/utils/links.ts"
            ));
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn search_suggestion_component_contract() {
        let scan = count_in_frontend(&["SearchSuggestion.svelte"]);
        let component = Path::new(SUGGESTION);
        let importers: Vec<&PathBuf> = scan.hits[0]
            .iter()
            .map(|(file, _)| file)
            .filter(|file| file.as_path() != component)
            .collect();
        if !scan.files.iter().any(|file| file == component) {
            println!("component absent; importers: {}", importers.len());
            assert!(
                importers.is_empty(),
                "{importers:?} import a component that does not exist"
            );
            return;
        }
        println!("component present; importers: {}", importers.len());
        let text = std::fs::read_to_string(scan.root.join(component)).unwrap();
        let mut failures = Vec::new();
        let mut check = |what: &str, holds: bool| {
            println!("{what}: {}", if holds { "ok" } else { "FAILED" });
            if !holds {
                failures.push(what.to_owned());
            }
        };
        check(
            "exactly one attachShadow( call",
            text.matches("attachShadow(").count() == 1,
        );
        let options = text
            .split_once("attachShadow(")
            .map(|(_, rest)| rest.split_once(')').map_or(rest, |(options, _)| options));
        check(
            "the shadow root is closed",
            options.is_some_and(|options| options.contains("mode: 'closed'")),
        );
        check(
            "the host is contained",
            text.contains("contain: layout paint"),
        );
        check(
            "links open through openLink",
            text.lines().any(|line| {
                line.contains("import")
                    && line.contains("openLink")
                    && (line.contains("utils/links'") || line.contains("utils/links\""))
            }),
        );
        for event in ["'click'", "'auxclick'", "'contextmenu'"] {
            check(&format!("listens for {event}"), text.contains(event));
        }
        for banned in ["<iframe", "sanitize", "'open_url'"] {
            check(&format!("no {banned}"), !text.contains(banned));
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
