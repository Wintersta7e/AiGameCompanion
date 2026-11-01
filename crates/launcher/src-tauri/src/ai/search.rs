//! The report of an answer's web search, stored with the answer.

use serde::{Deserialize, Serialize};

/// What an answer's web search did: the queries it ran, the pages it drew
/// on, and the search suggestion the provider asks to be shown with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchReport {
    pub(crate) searched: bool,
    pub(crate) queries: Vec<String>,
    pub(crate) sources: Vec<SearchSource>,
    pub(crate) suggestion_html: String,
    pub(crate) note: SearchNote,
    pub(crate) hints: bool,
}

/// One page an answer drew on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchSource {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) host: String,
}

/// What the report says beside its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SearchNote {
    /// Nothing to add.
    None,
    /// The search ran but named no page.
    QueriesOnly,
    /// The search failed.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_report_mirrors_rust() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/utils/search.ts"
        ));
        let report = SearchReport {
            searched: true,
            queries: vec!["foo boss weakness".to_owned()],
            sources: vec![SearchSource {
                title: "Foo guide".to_owned(),
                url: "https://example.com/foo".to_owned(),
                host: "example.com".to_owned(),
            }],
            suggestion_html: "<div>foo boss weakness</div>".to_owned(),
            note: SearchNote::QueriesOnly,
            hints: true,
        };
        let json = serde_json::to_value(&report).unwrap();

        let rust = crate::util::json_fields(&json);
        let typescript = crate::util::ts_fields(source, "interface SearchReport {");
        println!("SearchReport: Rust {rust:?}");
        println!("SearchReport: TypeScript {typescript:?}");
        assert!(
            !rust.is_empty() && !typescript.is_empty(),
            "no fields read from Rust or TypeScript"
        );
        // No field is an Option, so the same list stands for both samples.
        let mismatches = crate::util::mirror_mismatches(&typescript, &rust, &rust, source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        let rust = crate::util::json_fields(&serde_json::to_value(&report.sources[0]).unwrap());
        let typescript = crate::util::ts_fields(source, "interface SearchSource {");
        println!("SearchSource: Rust {rust:?}, TypeScript {typescript:?}");
        assert!(
            !rust.is_empty() && !typescript.is_empty(),
            "no fields read from Rust or TypeScript"
        );
        let mismatches = crate::util::mirror_mismatches(&typescript, &rust, &rust, source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        let notes: Vec<String> = [
            SearchNote::None,
            SearchNote::QueriesOnly,
            SearchNote::Failed,
        ]
        .into_iter()
        .map(|note| {
            // A new variant fails to compile here; add it to the list too.
            match note {
                SearchNote::None | SearchNote::QueriesOnly | SearchNote::Failed => {}
            }
            serde_json::to_value(note)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
        let names: Vec<&str> = notes.iter().map(String::as_str).collect();
        println!("SearchNote: {names:?}");
        let mismatches = crate::util::alias_mismatches(source, "SearchNote", &names);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        let back: SearchReport = serde_json::from_value(json).unwrap();
        assert_eq!(
            back, report,
            "the report did not read back as it was written"
        );
    }
}
