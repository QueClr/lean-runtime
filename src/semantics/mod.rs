//! Lean's runtime semantics on views and plain data: hashing, float and
//! character formatting, the fixed-width integer rows, `Nat`/`Int` rules (over
//! a big-number trait each translator implements), string-position
//! algorithms on UTF-8 bytes and array edge rules. Nothing here owns or
//! allocates a Lean value.
//!
//! Empty until the extraction step (see CONTRIBUTING.md, "Sequence").
