//! Yonder agent. Becomes a Windows service in Step 2; for now it only reports its version.

use clap::Parser;

/// Runs on a machine you want to use remotely.
#[derive(Debug, Parser)]
#[command(name = "yonder-agent", version, about)]
struct Args {}

fn main() {
    let _args = Args::parse();
    println!(
        "yonder-agent {} on {} (protocol {})",
        yonder_core::VERSION,
        yonder_core::Os::current(),
        yonder_core::PROTOCOL_VERSION
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }
}
