#![cfg(test)]
//! Lint (issue #956): every persistent-storage write must go through the
//! `set_persistent` helper, so no persistent key can be created or updated
//! at the minimum TTL.
//!
//! Both spellings are checked: the direct chain
//! `env.storage().persistent().set(` and the local-binding form
//! `let persistent = env.storage().persistent(); persistent.set(`.
//! Comments are stripped first so the helper's own documentation (which
//! quotes the call) cannot satisfy the check.

extern crate std;

use std::prelude::rust_2021::*;

const SRC: &str = include_str!("../lib.rs");

/// Source with every comment line removed, so documentation cannot be
/// mistaken for code.
fn code_without_comments() -> String {
    SRC.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Source with all whitespace removed, so a chain split over several lines
/// still matches the single-token form.
fn compact(source: &str) -> String {
    source.chars().filter(|c| c.is_whitespace()).collect()
}

/// Byte range of the `set_persistent` function body in the compacted code.
fn set_persistent_body(code: &str) -> Option<(usize, usize)> {
    let start = code.find("fnset_persistent<V>")?;
    let end = code[start..].find('}')? + start;
    Some((start, end))
}

#[test]
fn every_persistent_write_goes_through_set_persistent() {
    let code = compact(&code_without_comments());
    let (helper_start, helper_end) =
        set_persistent_body(&code).expect("set_persistent helper not found in lib.rs");

    // Direct chain form: `persistent().set(` — exactly one occurrence,
    // inside the helper.
    let needle = "persistent().set(";
    let mut rest = code.as_str();
    let mut occurrences = 0usize;
    while let Some(pos) = rest.find(needle) {
        let absolute = code.len() - rest.len() + pos;
        assert!(
            absolute > helper_start && absolute < helper_end,
            "direct `persistent().set(` call outside `set_persistent` at char {} \
             of lib.rs; route the write through the helper so the entry's \
             TTL is extended (issue #956)",
            absolute
        );
        occurrences += 1;
        rest = &rest[pos + needle.len()..];
    }
    assert_eq!(
        occurrences, 1,
        "expected exactly one `persistent().set(` call, inside `set_persistent`"
    );

    // Local-binding form: `persistent.set(` — none at all; every such call
    // site was converted to the helper.
    let local_needle = "persistent.set(";
    assert!(
        !code.contains(local_needle),
        "a local `persistent.set(` call survived the #956 conversion; \
         route the write through `set_persistent`"
    );
}

/// The helper must actually extend the TTL it promises, not just write.
#[test]
fn set_persistent_extends_ttl_after_writing() {
    let code = compact(&code_without_comments());
    let (helper_start, helper_end) =
        set_persistent_body(&code).expect("set_persistent helper not found in lib.rs");
    let body = &code[helper_start..helper_end];
    assert!(
        body.contains(".set(key,value);"),
        "set_persistent must write the value under `key`"
    );
    assert!(
        body.contains(".extend_ttl(key,TTL_THRESHOLD,TTL_EXTEND_TO"),
        "set_persistent must extend the entry's TTL with the shared \
         TTL_THRESHOLD / TTL_EXTEND_TO constants"
    );
}
