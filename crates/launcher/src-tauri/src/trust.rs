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
}
