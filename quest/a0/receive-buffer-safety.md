# [M] Make safe receive defaults sound

## Goal

Safe implementations of the async and poll receive traits cannot observe uninitialized memory or cause unchecked buffer advancement through the default helpers. Preserve the public receive APIs.

## Plan

The default helpers in `rs/web-transport-trait/src/lib.rs` and `src/poll.rs` transmute `UninitSlice` to `&mut [u8]` before invoking arbitrary safe implementations. A reader may legally inspect that slice. This is a source-level soundness finding; reproduce it under Miri before fixing it.

Initialize the destination before exposing a byte slice, or use initialized scratch storage and a checked copy. Validate the reported length before any unsafe advancement; preserve efficient backend overrides. Cover partial progress, cancellation/Pending, EOF, empty destinations and zero-sized chunk requests consistently across both traits. Include a safe reader that inspects the destination and a reader reporting an invalid length. Run the regression without the fix to establish sensitivity, then focused native/WASM checks and Miri. Avoid a public API redesign or unrelated stream refactor.
