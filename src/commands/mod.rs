//! CLI subcommand orchestration — one module per `burst <verb>`, thin over
//! the `cloud`/`github`/`payload` seams so the pure logic there stays
//! testable offline.

pub mod bake;
pub mod down;
pub mod image;
pub mod init;
pub mod status;
pub mod sweep;
pub mod up;
