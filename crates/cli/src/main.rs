//! `yonder` command line. Subcommands (`pair`, `machines`, `run`, …) arrive in Step 3.

use clap::Parser;

/// Run work on your other machines.
#[derive(Debug, Parser)]
#[command(name = "yonder", version, about)]
struct Args {}

fn main() {
    let _args = Args::parse();
    println!(
        "yonder {} (protocol {})",
        yonder_core::VERSION,
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
