# [S] Align workspace documentation with the current architecture

## Goal

The root and Rust package documentation correctly link the workspace and describe the current async/poll and WASM trait support.

## Plan

Repair root crate links that omit `rs/`, the router README claim that WASM cannot implement the trait, and the trait README's obsolete Quiche/sans-I/O TODO descriptions. Check remaining package maps against Cargo workspace membership and actual exports. Preserve package-local detail rather than growing AGENTS.md.

Binding-specific installation/build instructions are owned by the corresponding binding CI quests to avoid competing edits. Validate local links and examples against the source. This cleanup is independently shippable from the new package-selection guide.

## Related

- [Package-selection guide](/quest/a1/package-selection.md) - new comparative guidance
- [Binding runtime coverage](/quest/a1/bindings/README.md) - owns binding-local command corrections
