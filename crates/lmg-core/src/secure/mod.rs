//! The sealed state format. `envelope.rs` is the frozen on-disk format every state file is
//! written in (docs/05 §1); `key.rs` resolves the master key from the OS credential store;
//! `statefile.rs` reads/writes sealed state; `envstore.rs` is the encrypted .env replacement.

pub mod envelope;
pub mod envstore;
pub mod key;
pub mod statefile;
