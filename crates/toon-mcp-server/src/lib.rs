// file: crates/toon-mcp-server/src/lib.rs
// description: Library entry point — re-exports server internals for integration tests

#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! Library entry point for the `toon-mcp-server` crate.
//!
//! Exposes configuration, error types, tool handlers, and the MCP server
//! struct so that integration tests and the binary can share the same code.

/// Server configuration loaded from environment variables.
pub mod config;

/// Error types for the server binary.
pub mod error;

/// MCP tool handler functions and their input/output types.
pub mod handler;

/// rmcp `ServerHandler` implementation for the TOON MCP server.
pub mod server;
