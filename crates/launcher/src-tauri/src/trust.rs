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
    use tauri::http::HeaderMap;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY};
    use tauri::webview::InvokeRequest;
    use tauri::{Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

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

    // The only copy of each window's expected grants. A command joins these
    // lists, `APP_COMMANDS` in build.rs and the capability of each window whose
    // UI calls it in the change that adds its first caller.

    /// App commands the main window's UI calls.
    const MAIN_APP: &[&str] = &[
        "get_games",
        "scan_games",
        "launch_game",
        "open_game_logs",
        "open_config_folder",
        "update_settings",
        "state_health",
        "set_gemini_key",
        "recheck_clis",
        "hotkey_status",
        "open_url",
        "get_settings",
        "available_providers",
        "set_active_provider",
    ];
    /// App commands the overlay's UI calls.
    const OVERLAY_APP: &[&str] = &[
        "get_settings",
        "available_providers",
        "set_active_provider",
        "ask_sage",
        "cancel_sage",
        "translate_screen",
        "hide_overlay",
        "link_game",
    ];
    /// Core permissions of the main window.
    const MAIN_CORE: &[&str] = &[
        "core:default",
        "core:window:allow-minimize",
        "core:window:allow-toggle-maximize",
        "core:window:allow-close",
        "core:window:allow-start-dragging",
        "core:window:allow-set-focus",
    ];
    /// Core permissions of the overlay: its event listeners and titlebar drag.
    const OVERLAY_CORE: &[&str] = &[
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "core:window:allow-start-dragging",
    ];
    const OVERLAY_DESCRIPTION: &str =
        "Overlay: only the commands its UI calls; no file or folder opens.";

    /// The command names registered in `main.rs`'s `generate_handler!`.
    fn handler_commands() -> Vec<String> {
        let (_, list) = include_str!("main.rs")
            .split_once("generate_handler![")
            .expect("main.rs registers no command handlers");
        let (list, _) = list
            .split_once(']')
            .expect("the handler list is not closed");
        list.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.rsplit("::").next().unwrap_or(entry).to_owned())
            .collect()
    }

    /// The command names in build.rs's app manifest.
    fn manifest_commands() -> Vec<String> {
        let (_, declaration) = include_str!("../build.rs")
            .split_once("const APP_COMMANDS")
            .expect("build.rs declares no APP_COMMANDS");
        let (_, value) = declaration
            .split_once('=')
            .expect("APP_COMMANDS has no value");
        let (_, list) = value.split_once('[').expect("APP_COMMANDS is not a list");
        let (list, _) = list.split_once(']').expect("APP_COMMANDS is not closed");
        list.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    }

    /// The permission that grants `command`: `allow-` plus its name in kebab case.
    fn allow(command: &str) -> String {
        format!("allow-{}", command.replace('_', "-"))
    }

    #[test]
    fn app_manifest_matches_handlers() {
        let handlers = handler_commands();
        let manifest = manifest_commands();
        println!("handlers: {}, manifest: {}", handlers.len(), manifest.len());
        let handler_set: BTreeSet<&String> = handlers.iter().collect();
        let manifest_set: BTreeSet<&String> = manifest.iter().collect();
        assert!(
            !handlers.is_empty() && !manifest.is_empty(),
            "a list is empty"
        );
        assert_eq!(
            handler_set.len(),
            handlers.len(),
            "duplicate handler: {handlers:?}"
        );
        assert_eq!(
            manifest_set.len(),
            manifest.len(),
            "duplicate manifest name: {manifest:?}"
        );
        assert_eq!(handler_set, manifest_set);
    }

    #[test]
    fn capabilities_grant_exact_lists() {
        let mut dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/capabilities"));
        // Same reason as `util::sources_root`: tests run from the package root.
        if !dir.is_dir() {
            dir = PathBuf::from("capabilities");
        }
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        println!("{} capability files: {files:?}", files.len());
        assert_eq!(files, ["default.json", "overlay.json"]);

        let manifest = manifest_commands();
        let mut failures = Vec::new();
        let mut granted = BTreeSet::new();
        let mut counts = Vec::new();
        let windows = [
            ("default.json", "main", MAIN_APP, MAIN_CORE),
            ("overlay.json", "overlay", OVERLAY_APP, OVERLAY_CORE),
        ];
        for (file, window, app, core) in windows {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            let capability: serde_json::Value = serde_json::from_str(&text).unwrap();
            if capability["windows"] != serde_json::json!([window]) {
                failures.push(format!("{file}: windows {}", capability["windows"]));
            }
            if capability.get("remote").is_some() {
                failures.push(format!("{file}: grants remote pages"));
            }
            let mut permissions: Vec<String> = capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .map_or_else(|| entry.to_string(), str::to_owned)
                })
                .collect();
            permissions.sort();
            let mut expected: Vec<String> = core
                .iter()
                .map(|&entry| entry.to_owned())
                .chain(app.iter().map(|command| allow(command)))
                .collect();
            expected.sort();
            if permissions != expected {
                failures.push(format!("{file}: {permissions:?}, expected {expected:?}"));
            }
            for entry in &permissions {
                if entry.starts_with("allow-") {
                    granted.insert(entry.clone());
                    if !manifest.iter().any(|command| allow(command) == *entry) {
                        failures.push(format!("{file}: {entry} names no app command"));
                    }
                }
            }
            if window == "overlay" {
                for entry in permissions
                    .iter()
                    .filter(|entry| entry.ends_with(":default"))
                {
                    failures.push(format!("{file}: {entry} grants a whole default set"));
                }
                if capability["description"] != OVERLAY_DESCRIPTION {
                    failures.push(format!("{file}: description {}", capability["description"]));
                }
            }
            let app_count = permissions
                .iter()
                .filter(|entry| entry.starts_with("allow-"))
                .count();
            counts.push(format!(
                "{window}: {app_count} app + {} core",
                permissions.len() - app_count
            ));
        }
        for command in &manifest {
            if !granted.contains(&allow(command)) {
                failures.push(format!("{command} is granted to no window"));
            }
        }
        println!("{}", counts.join(", "));
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The page policy both windows load under.
    const CSP: &str = "default-src 'self' https://tauri.localhost; img-src 'self' https://cdn.cloudflare.steamstatic.com; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; script-src 'self' https://tauri.localhost; connect-src ipc: http://ipc.localhost https://tauri.localhost; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'";

    fn tauri_config() -> serde_json::Value {
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap()
    }

    #[test]
    fn overlay_window_cannot_maximize() {
        let config = tauri_config();
        let overlay = config["app"]["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|window| window["label"] == "overlay")
            .expect("tauri.conf.json has no overlay window");
        println!("overlay maximizable: {}", overlay["maximizable"]);
        assert_eq!(overlay["maximizable"], false);
    }

    #[test]
    fn csp_is_exact() {
        let config = tauri_config();
        let security = config["app"]["security"].as_object().unwrap();
        let keys: Vec<&String> = security.keys().collect();
        println!("security keys: {keys:?}");
        println!("csp: {}", security["csp"]);
        assert_eq!(keys, ["csp"]);
        assert_eq!(security["csp"], CSP);
    }

    /// Core and plugin commands the overlay must be refused.
    const OVERLAY_REFUSED: &[&str] = &[
        "plugin:window|internal_toggle_maximize",
        "plugin:window|maximize",
        "plugin:window|set_position",
        "plugin:window|set_size",
        "plugin:window|hide",
        "plugin:window|show",
        "plugin:window|set_focus",
        "plugin:window|close",
        "plugin:image|from_path",
        "plugin:tray|new",
        "plugin:menu|new",
        "plugin:webview|create_webview_window",
        "plugin:event|emit",
    ];
    /// Core commands the overlay's own UI needs.
    const OVERLAY_ALLOWED: &[&str] = &[
        "plugin:event|listen",
        "plugin:event|unlisten",
        "plugin:window|start_dragging",
    ];
    /// Plugin commands no window may call: the opener runs only from Rust.
    const ALL_REFUSED: &[&str] = &["plugin:opener|open_url", "plugin:opener|open_path"];

    #[test]
    #[allow(
        clippy::exit,
        reason = "tauri's generated context exits the process if building it panics"
    )]
    fn acl_denies_everything_unlisted() {
        let app = mock_builder()
            .build(tauri::generate_context!(test = true))
            .unwrap();
        // The app's own origin: any other URL is remote and would be refused
        // for that reason instead.
        let url: Url = if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
        .parse()
        .unwrap();
        let denied = |window: &WebviewWindow<MockRuntime>, cmd: &str| {
            let request = InvokeRequest {
                cmd: cmd.to_owned(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: url.clone(),
                body: InvokeBody::default(),
                headers: HeaderMap::default(),
                invoke_key: INVOKE_KEY.to_owned(),
            };
            get_ipc_response(window, request)
                .is_err_and(|error| error.to_string().contains("not allowed"))
        };

        let handlers = handler_commands();
        let mut mismatches = Vec::new();
        let mut summary = Vec::new();
        for (label, listed) in [("main", MAIN_APP), ("overlay", OVERLAY_APP)] {
            // `build` does not create the configured windows (the run loop
            // does), and capabilities match windows by label.
            let window = WebviewWindowBuilder::new(&app, label, WebviewUrl::default())
                .build()
                .unwrap();
            let mut probes: Vec<(&str, bool)> =
                ALL_REFUSED.iter().map(|&cmd| (cmd, true)).collect();
            if label == "overlay" {
                probes.extend(OVERLAY_REFUSED.iter().map(|&cmd| (cmd, true)));
                probes.extend(OVERLAY_ALLOWED.iter().map(|&cmd| (cmd, false)));
            }
            let mut denials = 0;
            for command in &handlers {
                let refused = denied(&window, command);
                denials += usize::from(refused);
                if refused == listed.contains(&command.as_str()) {
                    mismatches.push(format!("{label}: {command} refused: {refused}"));
                }
            }
            for &(cmd, expected) in &probes {
                let refused = denied(&window, cmd);
                denials += usize::from(refused);
                if refused != expected {
                    mismatches.push(format!("{label}: {cmd} refused: {refused}"));
                }
            }
            summary.push(format!(
                "{label}: {} commands + {} probes, {denials} refused",
                handlers.len(),
                probes.len()
            ));
        }
        println!("{}", summary.join("; "));
        assert!(mismatches.is_empty(), "{mismatches:#?}");
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
