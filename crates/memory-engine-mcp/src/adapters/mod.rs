//! One adapter per harness (ADR-0020 §1): it turns one harness body into one neutral record,
//! and is chosen by a `match` on `ME-Harness`. There is no adapter trait until a second adapter
//! exists.

pub mod claude_code;
