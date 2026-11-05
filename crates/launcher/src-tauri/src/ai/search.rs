//! The report of an answer's web search, stored with the answer, and the
//! gate that decides whether a request may search.

use serde::{Deserialize, Serialize};

use super::Provider;

/// Whether a request searches the web: never for a recap, which answers from
/// the chat alone. No provider can search here yet, so a search request is
/// refused with the text the overlay shows.
pub(crate) fn search_gate(provider: Provider, search: bool, recap: bool) -> Result<bool, String> {
    if !search || recap {
        return Ok(false);
    }
    Err(format!(
        "{} can't search the web here. Turn off Search to ask without it.",
        provider.label()
    ))
}

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

/// The largest search report kept with an answer, in bytes of compact JSON.
pub(crate) const MAX_SEARCH_REPORT_BYTES: usize = 32 * 1024;

/// What a saved answer drawn from a web search is sent again as, to a
/// provider other than the one that searched.
pub(crate) const OMITTED_ANSWER: &str = "[answer omitted]";

/// Whether an answer came from a Gemini web search: only such an answer is
/// shown with its search suggestion.
pub(crate) const fn is_grounded(report: &SearchReport) -> bool {
    !report.suggestion_html.is_empty()
}

/// The text a saved answer is sent again as: Gemini's search results go back
/// to Gemini only, so another provider gets `OMITTED_ANSWER` in their place.
pub(crate) fn resend_answer<'a>(
    next: Provider,
    answer: &'a str,
    report: Option<&SearchReport>,
) -> &'a str {
    if matches!(next, Provider::Gemini) || !report.is_some_and(is_grounded) {
        answer
    } else {
        OMITTED_ANSWER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_gate_table() {
        for provider in [Provider::Gemini, Provider::Claude, Provider::Openai] {
            for search in [false, true] {
                for recap in [false, true] {
                    let gate = search_gate(provider, search, recap);
                    println!("{provider:?}, search {search}, recap {recap}: {gate:?}");
                    if search && !recap {
                        let Err(text) = gate else {
                            panic!("{provider:?} was let search");
                        };
                        assert!(text.contains(provider.label()), "{text}");
                    } else {
                        assert_eq!(
                            gate,
                            Ok(false),
                            "{provider:?}, search {search}, recap {recap}"
                        );
                    }
                }
            }
        }
    }

    fn report(searched: bool, suggestion_html: &str) -> SearchReport {
        SearchReport {
            searched,
            queries: Vec::new(),
            sources: Vec::new(),
            suggestion_html: suggestion_html.to_owned(),
            note: SearchNote::None,
            hints: false,
        }
    }

    #[test]
    fn grounded_answers_resend_only_to_gemini() {
        let grounded = report(true, "<div>foo</div>");
        let searched_without_suggestion = report(true, "");
        let not_searched = report(false, "");
        assert!(
            is_grounded(&grounded),
            "a suggestion makes a report grounded"
        );
        assert!(
            !is_grounded(&searched_without_suggestion),
            "no suggestion: not grounded"
        );
        assert!(!is_grounded(&not_searched), "no search: not grounded");

        let answer = "Go north.";
        for next in [Provider::Gemini, Provider::Claude, Provider::Openai] {
            for (name, report) in [
                ("grounded", Some(&grounded)),
                (
                    "searched, no suggestion",
                    Some(&searched_without_suggestion),
                ),
                ("not searched", Some(&not_searched)),
                ("no report", None),
            ] {
                let sent = resend_answer(next, answer, report);
                println!("{next:?}, {name}: {sent:?}");
                let expected = if name == "grounded" && !matches!(next, Provider::Gemini) {
                    OMITTED_ANSWER
                } else {
                    answer
                };
                assert_eq!(sent, expected, "{next:?}, {name}");
            }
        }
    }

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
