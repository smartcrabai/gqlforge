#![expect(
    clippy::module_inception,
    reason = "submodule intentionally mirrors the parent module name"
)]

pub mod config;
mod generator;
mod source;

pub use generator::Generator;
