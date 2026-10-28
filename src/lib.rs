//! Safe, structured Model Context Protocol server for Moonraker / Klipper.
//!
//! The crate is deliberately split into layers:
//!
//! * [`config`] resolves defaults, an optional TOML file, and environment
//!   variables into a single validated configuration.
//! * [`moonraker`] is a thin, typed transport for the Moonraker JSON-RPC API.
//! * [`camera`] discovers webcams through Moonraker and fetches snapshots with
//!   SSRF and size guards.
//! * [`safety`] owns the policy engine, secret redaction, and the
//!   single-use approval store used for destructive actions.
//! * [`tools`] implements each MCP tool on top of those layers.
//! * [`server`] and [`transport`] wire the tools into the MCP runtime.
//!
//! Read [`config`] first if you are deploying this server; it documents every
//! environment variable and the safe defaults.

pub mod audit;
pub mod camera;
pub mod config;
pub mod error;
pub mod moonraker;
pub mod safety;
pub mod server;
pub mod tools;
pub mod transport;

pub use config::Config;
pub use error::{Error, Result};
