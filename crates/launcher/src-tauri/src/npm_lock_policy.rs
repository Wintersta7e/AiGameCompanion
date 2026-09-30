//! The committed npm lockfile's policy: every package is fetched from the npm
//! registry with a sha512 integrity and carries a license the allowlist
//! accepts.

use serde_json::Value;

/// The licenses an npm package may carry. Kept apart from `deny.toml`'s list:
/// the npm tree uses BSD-2-Clause, which no Rust crate here does.
const ALLOWED_LICENSES: [&str; 8] = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "MPL-2.0",
    "BlueOak-1.0.0",
    "0BSD",
];

/// Where every package is fetched from. The trailing slash keeps a look-alike
/// host that merely starts with the same name out.
const REGISTRY: &str = "https://registry.npmjs.org/";

/// A rule the lockfile can break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    Json,
    Version,
    Empty,
    Source,
    License,
}

impl Rule {
    /// The rule in words, for the failure message.
    const fn describe(self) -> &'static str {
        match self {
            Self::Json => "is not valid JSON",
            Self::Version => "is not 3",
            Self::Empty => "holds no package besides the root",
            Self::Source => "is not fetched from the npm registry with a sha512 integrity",
            Self::License => "has no license, or one the allowlist does not accept",
        }
    }
}

/// A broken rule: the lockfile key that breaks it, and the rule.
type Violation = (String, Rule);

/// Whether `expression` is one allowed license, or an `A OR B [OR ...]` list
/// naming at least one. `AND`, `WITH` and parentheses are never accepted.
fn license_allowed(expression: &str) -> bool {
    let single = |term: &str| {
        !term.is_empty() && !term.contains(|c: char| c.is_whitespace() || c == '(' || c == ')')
    };
    expression.split(" OR ").all(single)
        && expression
            .split(" OR ")
            .any(|term| ALLOWED_LICENSES.contains(&term))
}

/// Checks one package entry, adding what it breaks to `found`.
fn check_entry(key: &str, entry: &Value, found: &mut Vec<Violation>) {
    let text = |field: &str| entry.get(field).and_then(Value::as_str).unwrap_or_default();
    let bundled = entry.get("inBundle").and_then(Value::as_bool) == Some(true);
    let fetched =
        text("resolved").starts_with(REGISTRY) && text("integrity").starts_with("sha512-");
    if !bundled && !fetched {
        found.push((key.to_owned(), Rule::Source));
    }
    if !entry
        .get("license")
        .and_then(Value::as_str)
        .is_some_and(license_allowed)
    {
        found.push((key.to_owned(), Rule::License));
    }
}

/// Checks a lockfile's text: how many package entries it checked, and every
/// rule the file breaks.
fn check(text: &str) -> (usize, Vec<Violation>) {
    let Ok(lock) = serde_json::from_str::<Value>(text) else {
        return (0, vec![("(file)".to_owned(), Rule::Json)]);
    };
    let mut found = Vec::new();
    if lock.get("lockfileVersion").and_then(Value::as_u64) != Some(3) {
        found.push(("lockfileVersion".to_owned(), Rule::Version));
    }
    let mut checked = 0;
    let packages = lock.get("packages").and_then(Value::as_object);
    for (key, entry) in packages.into_iter().flatten() {
        if !key.is_empty() {
            checked += 1;
            check_entry(key, entry, &mut found);
        }
    }
    if checked == 0 {
        found.push(("packages".to_owned(), Rule::Empty));
    }
    (checked, found)
}

/// One line per violation: the lockfile key, then the rule in words.
fn describe(found: &[Violation]) -> String {
    found
        .iter()
        .map(|(key, rule)| format!("\n  {key}: {}", rule.describe()))
        .collect::<Vec<_>>()
        .concat()
}

#[test]
fn committed_lock_meets_the_policy() {
    let (checked, found) = check(include_str!("../../package-lock.json"));
    assert!(
        checked > 0 && found.is_empty(),
        "package-lock.json breaks the lockfile policy ({checked} package entries checked):{}",
        describe(&found)
    );
}

/// The planted package's key in every fixture below.
const PLANTED: &str = "node_modules/fixture-pkg";

/// A whole lockfile holding the root and one package, `entry`.
fn planted(entry: &str) -> String {
    [
        r#"{"lockfileVersion": 3, "packages": {"": {"name": "fixture"}, "node_modules/fixture-pkg": "#,
        entry,
        "}}",
    ]
    .concat()
}

/// Planted package entries, each breaking exactly one rule.
const REJECTED: [(&str, Rule); 14] = [
    (
        r#"{"resolved": "https://packages.example.invalid/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT"}"#,
        Rule::Source,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org.example.invalid/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT"}"#,
        Rule::Source,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha1-ZmFrZQ==", "license": "MIT"}"#,
        Rule::Source,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "license": "MIT"}"#,
        Rule::Source,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "GPL-3.0"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT AND BSD-3-Clause"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "Apache-2.0 WITH LLVM-exception"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "(MIT OR Apache-2.0)"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "GPL-2.0 OR GPL-3.0"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ=="}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": {"type": "MIT"}}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "ISC OR (MIT)"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT OR ISC AND GPL-3.0"}"#,
        Rule::License,
    ),
    (
        r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT OR GPL-2.0 WITH Classpath-exception-2.0"}"#,
        Rule::License,
    ),
];

/// Planted package entries that break no rule.
const ACCEPTED: [&str; 4] = [
    r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT"}"#,
    r#"{"inBundle": true, "license": "MIT"}"#,
    r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "Apache-2.0 OR MIT"}"#,
    r#"{"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "GPL-3.0 OR MIT"}"#,
];

#[test]
fn planted_locks_are_judged_by_each_rule() {
    let whole_files = [
        (
            r#"{"lockfileVersion": 2, "packages": {"": {"name": "fixture"}, "node_modules/fixture-pkg": {"resolved": "https://registry.npmjs.org/fixture-pkg/-/fixture-pkg-1.0.0.tgz", "integrity": "sha512-ZmFrZQ==", "license": "MIT"}}}"#.to_owned(),
            vec![("lockfileVersion".to_owned(), Rule::Version)],
        ),
        (
            r#"{"lockfileVersion": 3, "packages": {"": {"name": "fixture"}}}"#.to_owned(),
            vec![("packages".to_owned(), Rule::Empty)],
        ),
    ];
    let entries = REJECTED
        .iter()
        .map(|&(entry, rule)| (planted(entry), vec![(PLANTED.to_owned(), rule)]));
    let accepted = ACCEPTED.iter().map(|&entry| (planted(entry), Vec::new()));
    let mut mismatches = Vec::new();
    let mut cases = 0;
    for (text, expected) in whole_files.into_iter().chain(entries).chain(accepted) {
        cases += 1;
        let (checked, found) = check(&text);
        if found != expected || (expected.is_empty() && checked != 1) {
            mismatches.push(format!(
                "{text}\n  expected:{}\n  found:{}",
                describe(&expected),
                describe(&found)
            ));
        }
    }
    assert!(
        cases == 20 && mismatches.is_empty(),
        "{cases} planted lockfiles, {} judged wrongly:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
