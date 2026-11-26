#![cfg(test)]
//! Spec & README entry-point sync (issue #959).
//!
//! Every public entry point of `#[contractimpl] impl MilestoneEscrow` must
//! be documented in `contracts/milestone-escrow/SPEC.md` and in the
//! "Contract Functions" section of the root `README.md`. Modeled on
//! `event_docs_sync_tests`.

extern crate std;

use std::prelude::rust_2021::*;

const SRC: &str = include_str!("../lib.rs");
const SPEC: &str = include_str!("../../SPEC.md");
const README: &str = include_str!("../../../../README.md");

/// Entry points that are intentionally not documented per-function.
/// `__constructor` is the generated constructor; every other public
/// function of the contract impl is a callable entry point and must be
/// documented.
const ALLOWLIST: &[&str] = &["__constructor"];

/// The README's function reference, between the `## Contract Functions`
/// heading and the next `##` heading.
fn readme_contract_functions() -> &'static str {
    let start = README
        .find("## Contract Functions")
        .expect("README.md has no `## Contract Functions` section");
    let rest = &README[start..];
    let end = rest[1..].find("\n## ").map(|i| 1 + i).unwrap_or(rest.len());
    &rest[..end]
}

/// Collect every `pub fn` declared inside an `impl MilestoneEscrow` block,
/// skipping `#[cfg(test)]` items and `__constructor`.
fn public_entry_points() -> Vec<(String, usize)> {
    let mut names = Vec::new();
    let mut in_impl = false;
    let mut skip_next_fn = false;
    for (idx, line) in SRC.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[contractimpl]") {
            in_impl = true;
            continue;
        }
        if trimmed == "#[cfg(test)]" {
            // The attribute applies to the next item (fn or impl block).
            skip_next_fn = true;
            continue;
        }
        if trimmed.starts_with("impl MilestoneEscrow") {
            continue;
        }
        if line.starts_with('}') {
            in_impl = false;
            continue;
        }
        if in_impl {
            if trimmed.starts_with("pub fn ") {
                if !skip_next_fn {
                    if let Some(name) = trimmed
                        .strip_prefix("pub fn ")
                        .and_then(|rest| rest.split('(').next())
                    {
                        names.push((name.trim().to_string(), idx + 1));
                    }
                }
                skip_next_fn = false;
            } else if trimmed.starts_with("fn ") {
                skip_next_fn = false;
            }
        }
    }
    names
        .into_iter()
        .filter(|(name, _)| !ALLOWLIST.contains(&name.as_str()))
        .collect()
}

#[test]
fn every_public_entry_point_is_documented_in_spec() {
    let entry_points = public_entry_points();
    assert!(
        entry_points.len() > 100,
        "entry-point extraction looks broken; found {}",
        entry_points.len()
    );
    let mut missing: Vec<String> = Vec::new();
    for (name, line) in &entry_points {
        if !SPEC.contains(&format!("`{}`", name)) {
            missing.push(format!("{name} (lib.rs line {line})"));
        }
    }
    assert!(
        missing.is_empty(),
        "public entry points missing from contracts/milestone-escrow/SPEC.md:\n{}",
        missing.join("\n")
    );
}

#[test]
fn every_public_entry_point_is_documented_in_readme() {
    let functions = readme_contract_functions();
    assert!(
        functions.contains("### Core Escrow"),
        "README function section boundary looks wrong"
    );
    let entry_points = public_entry_points();
    let mut missing: Vec<String> = Vec::new();
    for (name, line) in &entry_points {
        if !functions.contains(&format!("`{}`", name)) {
            missing.push(format!("{name} (lib.rs line {line})"));
        }
    }
    assert!(
        missing.is_empty(),
        "public entry points missing from the README `Contract Functions` section:\n{}",
        missing.join("\n")
    );
}

#[test]
fn allowlist_is_not_stale() {
    // Every allowlisted name must still exist in the source; a renamed or
    // removed entry point should be dropped from the allowlist.
    for name in ALLOWLIST {
        assert!(
            SRC.contains(&format!("pub fn {name}(")),
            "allowlist entry `{name}` no longer exists in lib.rs"
        );
    }
}
