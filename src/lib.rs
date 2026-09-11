//! RailQ's application library.
//!
//! Future simulation code stays pure and receives explicit timestamps. The
//! binary owns terminal, clock, and disk I/O at the application boundary.

pub mod balance;
pub mod model;
pub mod sim;
pub mod storage;

/// The display name shared by the binary and future presentation code.
pub const APPLICATION_NAME: &str = "RailQ";
