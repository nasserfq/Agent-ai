//! Agent AI's engine (وكيل): talks to Claude, Gemini, OpenRouter and any OpenAI-compatible service, runs
//! agents with tools (built-in file and web tools, and MCP servers such as the ones that ship with
//! كلمة, جدول, عرض and وثيقة), and stores settings, keys and conversations. It has no interface
//! code, so it is tested headlessly.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod agent;
pub mod error;
pub mod i18n;
pub mod providers;
pub mod run;
pub mod secrets;
pub mod sse;
pub mod store;
pub mod tools;
pub mod types;

pub use error::{Error, Result};
