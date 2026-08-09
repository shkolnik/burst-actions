use clap::{ArgGroup, Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "burst",
    version,
    about = "Ephemeral cloud VMs as GitHub Actions runners"
)]
struct Cli {
    /// Target GitHub repository as owner/repo (overrides burst.toml)
    #[arg(long, global = true)]
    repo: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Launch runner VMs: a count, or --auto to size from the queued jobs
    #[command(group(ArgGroup::new("count_source").required(true).multiple(false)))]
    Up {
        /// Number of VMs to launch
        #[arg(group = "count_source")]
        n: Option<u32>,
        /// Size the fleet from the queued burst-labeled jobs
        #[arg(long, group = "count_source")]
        auto: bool,
        /// Use spot instances
        #[arg(long)]
        spot: bool,
        /// Skip interactive confirmations (for automation)
        #[arg(long)]
        yes: bool,
        /// EC2 key pair name to allow SSH debug access
        #[arg(long)]
        ssh_key: Option<String>,
    },
    /// Build or rebuild the runner AMI
    Bake,
    /// Show live fleet state (cloud truth)
    Status,
    /// Terminate this repo's fleet
    Down {
        #[arg(long)]
        yes: bool,
    },
    /// Reap expired instances, orphan schedules, dead registrations
    Sweep,
}

fn not_implemented(cmd: &str) -> ExitCode {
    eprintln!("burst {cmd}: not implemented yet (see implementation-phases.md)");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let config = match burst::config::load(&cwd, cli.repo.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cli.command {
        Cmd::Up { .. } => not_implemented("up"),
        Cmd::Bake => match burst::commands::bake::run(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Cmd::Status => not_implemented("status"),
        Cmd::Down { .. } => not_implemented("down"),
        Cmd::Sweep => not_implemented("sweep"),
    }
}
