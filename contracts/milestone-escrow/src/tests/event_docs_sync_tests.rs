extern crate std;

const SRC: &str = include_str!("../lib.rs");
const DOCS: &str = include_str!("../../../../EVENTS.md");

/// Heading that starts the escrow-factory section of `EVENTS.md`. The factory
/// is a separate contract, so its events are not emitted by `lib.rs` here.
const FACTORY_SECTION: &str = "\n## Escrow factory events";

/// The part of `EVENTS.md` that documents this contract's events.
fn escrow_docs() -> &'static str {
    DOCS.split(FACTORY_SECTION).next().unwrap_or(DOCS)
}

fn src_emits(topic: &str) -> bool {
    let needle = "symbol_short!(\"";
    let mut rest = SRC;
    while let Some(pos) = rest.find(needle) {
        let after = &rest[pos + needle.len()..];
        let end = match after.find('"') {
            Some(e) => e,
            None => return false,
        };
        if &after[..end] == topic {
            return true;
        }
        rest = &after[end..];
    }
    false
}

fn docs_mentions(topic: &str) -> bool {
    let mut rest = DOCS;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let close = match after.find('`') {
            Some(c) => c,
            None => return false,
        };
        if &after[..close] == topic {
            return true;
        }
        rest = &after[close + 1..];
    }
    false
}

/// Every `symbol_short!("...")` topic emitted by the contract must have a
/// row in `EVENTS.md`.
#[test]
fn every_source_event_topic_is_documented() {
    let needle = "symbol_short!(\"";
    let mut rest = SRC;
    let mut count = 0u32;
    while let Some(pos) = rest.find(needle) {
        let after = &rest[pos + needle.len()..];
        let end = match after.find('"') {
            Some(e) => e,
            None => break,
        };
        let topic = &after[..end];
        count += 1;
        assert!(
            docs_mentions(topic),
            "topic `{}` emitted by lib.rs is missing from EVENTS.md",
            topic
        );
        rest = &after[end..];
    }
    assert!(count > 0, "no symbol_short! topics found in lib.rs");
}

/// Every topic documented in the `EVENTS.md` table must actually be
/// emitted by the contract.
#[test]
fn every_documented_topic_exists_in_source() {
    let mut checked = 0u32;
    let mut missing = 0u32;
    for line in escrow_docs().lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("| `") {
            continue;
        }
        let rest = &trimmed[1..];
        let rest = match rest.find('`') {
            Some(i) => &rest[i + 1..],
            None => continue,
        };
        let end = match rest.find('`') {
            Some(e) => e,
            None => continue,
        };
        let topic = &rest[..end];
        checked += 1;
        if !src_emits(topic) {
            std::eprintln!(
                "EVENTS.md documents `{}`, which lib.rs does not emit",
                topic
            );
            missing += 1;
        }
    }
    assert!(checked > 0, "no topics found in the EVENTS.md table");
    assert_eq!(
        missing, 0,
        "EVENTS.md documents topics the contract does not emit"
    );
}
