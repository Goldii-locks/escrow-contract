extern crate std;

const SRC: &str = include_str!("../lib.rs");
const FACTORY_SRC: &str = include_str!("../../../escrow-factory/src/lib.rs");
const DOCS: &str = include_str!("../../../../EVENTS.md");

/// Heading that starts the escrow-factory section of `EVENTS.md`. The factory
/// is a separate contract, so its events are not emitted by `lib.rs` here.
const FACTORY_SECTION: &str = "\n## Escrow factory events";

/// The part of `EVENTS.md` that documents this contract's events.
fn escrow_docs() -> &'static str {
    DOCS.split(FACTORY_SECTION).next().unwrap_or(DOCS)
}

/// The part of `EVENTS.md` that documents the factory's events.
fn factory_docs() -> &'static str {
    DOCS.split(FACTORY_SECTION).nth(1).unwrap_or("")
}

fn src_emits(src: &str, topic: &str) -> bool {
    let needle = "symbol_short!(\"";
    let mut rest = src;
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

fn docs_mentions(docs: &str, topic: &str) -> bool {
    let mut rest = docs;
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

/// Every `symbol_short!("...")` topic emitted by a contract must have a row in
/// `EVENTS.md`.
fn check_every_source_topic_is_documented(src: &str, docs: &str, label: &str) {
    let needle = "symbol_short!(\"";
    let mut rest = src;
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
            docs_mentions(docs, topic),
            "topic `{}` emitted by {} is missing from EVENTS.md",
            topic,
            label
        );
        rest = &after[end..];
    }
    assert!(count > 0, "no symbol_short! topics found in {}", label);
}

/// Every topic documented in an `EVENTS.md` table must actually be emitted by
/// the contract that section describes.
fn check_every_documented_topic_exists_in_source(src: &str, docs: &str, label: &str) {
    let mut checked = 0u32;
    let mut missing = 0u32;
    for line in docs.lines() {
        // Only the summary table's first column contains event topics. Later
        // sections have data-field tables whose first column is backticked too.
        if line.starts_with("### ") {
            break;
        }
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
        if !src_emits(src, topic) {
            std::eprintln!(
                "EVENTS.md documents `{}`, which {} does not emit",
                topic,
                label
            );
            missing += 1;
        }
    }
    assert!(checked > 0, "no topics found in the {} docs table", label);
    assert_eq!(
        missing, 0,
        "{} docs document topics the contract does not emit",
        label
    );
}

/// Every `symbol_short!` topic published by a contract must carry exactly one
/// payload struct, so indexers can decode event data by topic alone.
fn published_payloads(src: &str) -> std::collections::BTreeMap<&str, &str> {
    let mut payloads: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    let mut rest = src;
    while let Some(pos) = rest.find(".publish(") {
        rest = &rest[pos + ".publish(".len()..];
        let call = rest.trim_start();
        if !call.starts_with('(') {
            continue;
        }

        // Find the end of the topic tuple.
        let mut depth = 0i32;
        let mut tuple_end = None;
        for (i, c) in call.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        tuple_end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let tuple_end = tuple_end.expect("unterminated publish topic tuple");
        let tuple = &call[..tuple_end];
        let needle = "symbol_short!(\"";
        let topic = match tuple.find(needle) {
            Some(t) => {
                let after = &tuple[t + needle.len()..];
                &after[..after.find('"').expect("unterminated topic")]
            }
            None => continue,
        };

        let payload = call[tuple_end + 1..]
            .trim_start()
            .strip_prefix(',')
            .expect("publish call is missing a payload")
            .trim_start();
        // The factory's `disallowd` event intentionally has an empty payload.
        if payload.starts_with("()") {
            if let Some(existing) = payloads.insert(topic, "()") {
                assert_eq!(
                    existing, "()",
                    "topic `{}` is published with two payload types",
                    topic
                );
            }
            continue;
        }
        let name_end = payload
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(payload.len());
        let name = &payload[..name_end];
        assert!(
            payload[name_end..].trim_start().starts_with('{'),
            "payload of topic `{}` is not a struct literal",
            topic
        );

        if let Some(existing) = payloads.insert(topic, name) {
            assert_eq!(
                existing, name,
                "topic `{}` is published with two payload types",
                topic
            );
        }
    }
    assert!(!payloads.is_empty(), "no published topics found in source");
    payloads
}

#[test]
fn every_source_event_topic_is_documented() {
    check_every_source_topic_is_documented(SRC, DOCS, "milestone-escrow");
}

#[test]
fn every_documented_topic_exists_in_source() {
    check_every_documented_topic_exists_in_source(SRC, escrow_docs(), "milestone-escrow");
}

#[test]
fn every_event_topic_has_one_payload_type() {
    published_payloads(SRC);
}

#[test]
fn every_factory_event_topic_is_documented() {
    check_every_source_topic_is_documented(FACTORY_SRC, factory_docs(), "escrow-factory");
}

#[test]
fn every_documented_factory_topic_exists_in_source() {
    check_every_documented_topic_exists_in_source(FACTORY_SRC, factory_docs(), "escrow-factory");
}

#[test]
fn every_factory_event_topic_has_one_payload_type() {
    published_payloads(FACTORY_SRC);
}
