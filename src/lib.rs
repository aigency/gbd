//! gbd: Beads workflow on GitHub Issues and Projects. `gh` is the transport.

pub mod beads;
pub mod cli;
pub mod config;
pub mod fields;
pub mod gh;
pub mod ids;
pub mod import;
pub mod init;
pub mod issue;
pub mod memory;
pub mod project;
pub mod ready;
pub mod render;
pub mod repo;

pub fn run() -> anyhow::Result<u8> {
    cli::run()
}
