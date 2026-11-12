//! Lejce ("reins"): one way to drive several coding agents.
//!
//! An application tells Lejce which engine to run (Claude Code, pi, later
//! Codex), where (this machine, or a folder on another machine over SSH),
//! and which MCP servers the engine gets. Lejce starts the engine, sends it
//! messages, and turns its output into one stream of events: text,
//! thinking, tool calls, questions for the user, token usage.
//!
//! Kuterlapa and Lemat both use it. See `docs/design.md` for the plan.

pub mod host;
pub mod usage;

pub use host::Host;
pub use usage::Call;
