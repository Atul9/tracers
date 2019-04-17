//! This is the main module for all code generators, both the build-time generators invoked from
//! `build.rs` and the generators used by the proc macros.  There are multiple implementations of
//! these generators for the various tracing implementations, though only one can be active at
//! compile time, via conditonal compilation
//mod c;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub mod stap_usdt;
