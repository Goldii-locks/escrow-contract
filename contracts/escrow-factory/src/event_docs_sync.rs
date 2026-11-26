//! Keeps the "Escrow factory events" section of EVENTS.md in sync with the
//! topics this contract publishes (#1086), mirroring the escrow's
//! `event_docs_sync_tests`.

extern crate std;

const SRC: &str = include_str!("lib.rs");
const DOCS: &str = include_str!("../../../EVENTS.md");
const FACTORY_SECTION: &str = "\n## Escrow factory events";

/// The factory section of EVENTS.md, up to the next top-level heading.
fn factory_docs() -> &'static str {
    let start = DOCS
        .find(FACTORY_SECTION)
        .expect("EVENTS.md has no \"## Escrow factory events\" section");
    let section = &DOCS[start + FACTORY_SECTION.len()..];
    match section.find("\n## ") {
        Some(end) => &section[..end],
        None => section,
    }
}

/// Topics passed to `symbol_short!` in lib.rs.
fn source_topics() -> std::vec::Vec<&'static str> {
    let needle = "symbol_short!(\"";
    let mut topics = std::vec::Vec::new();
    let mut rest = SRC;
    while let Some(pos) = rest.find(needle) {
        let after = &rest[pos + needle.len()..];
        let end = after.find('"').expect("unterminated symbol_short!");
        topics.push(&after[..end]);
        rest = &after[end..];
    }
    topics
}

/// Topics in the first column of the factory section's tables.
fn documented_topics() -> std::vec::Vec<&'static str> {
    let mut topics = std::vec::Vec::new();
    for line in factory_docs().lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("| `") {
            continue;
        }
        let cell = &trimmed[3..];
        if let Some(end) = cell.find('`') {
            topics.push(&cell[..end]);
        }
    }
    topics
}

#[test]
fn every_factory_topic_is_documented() {
    let documented = documented_topics();
    let topics = source_topics();
    assert!(
        !topics.is_empty(),
        "no symbol_short! topics found in the factory"
    );
    for topic in topics {
        assert!(
            documented.contains(&topic),
            "factory topic `{}` has no row in the EVENTS.md factory section",
            topic
        );
    }
}

#[test]
fn every_documented_factory_topic_is_emitted() {
    let topics = source_topics();
    let documented = documented_topics();
    assert!(
        documented.contains(&"created"),
        "`created` must be documented"
    );
    for topic in documented {
        // Field tables in the section also start with a backticked cell;
        // only event-name rows are topics, and those are short symbols.
        if !topics.contains(&topic) && documented_is_event_row(topic) {
            panic!(
                "EVENTS.md factory section documents `{}`, which the factory does not emit",
                topic
            );
        }
    }
}

/// Event rows are the ones whose second column names an event struct or `none`.
fn documented_is_event_row(topic: &str) -> bool {
    factory_docs().lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with(&std::format!("| `{}`", topic))
            && (trimmed.contains("Event`") || trimmed.contains("none (data"))
    })
}
