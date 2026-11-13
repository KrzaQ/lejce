//! Lejce ("reins"): one way to drive several coding agents.
//!
//! An application tells Lejce which engine to run (Claude Code, pi or
//! Codex), where (this machine, or a folder on another machine over SSH),
//! and which MCP servers the engine gets. Lejce starts the engine, sends it
//! messages, and turns its output into one stream of events: text,
//! thinking, tool calls, questions for the user, token usage.
//!
//! Kuterlapa and Lemat both use it. See `docs/design.md` for the plan.

pub mod activity;
mod claude;
mod codex;
pub mod engine;
pub mod host;
pub mod mcp;
mod pi;
pub mod question;
pub mod run;
pub mod usage;

pub use activity::Activity;
pub use engine::Engine;
pub use host::{Forward, Host};
pub use mcp::{McpServer, Transport};
pub use question::{Answer, Question};
pub use run::{
    cancel_all, new_session_id, run, Canceller, Conversation, Error, Outcome, Permissions, Session,
    Setup, Tools,
};
pub use usage::Call;
