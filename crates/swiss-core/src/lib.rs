//! The floor layer every crate stands on: paths, logging, small utilities, the sealed-envelope
//! format and the platform seam. No gateway concepts here — nothing in swiss-core knows an MCP
//! exists.

pub mod atomic_json;
pub mod env;
pub mod log;
pub mod paths;
pub mod platform;
pub mod secure;
pub mod util;
