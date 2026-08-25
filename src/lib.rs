#![recursion_limit = "256"]

#[cfg(feature = "cli")]
mod allocator;
pub mod core;

#[cfg(feature = "cli")]
pub mod cli;
