use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
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
    /// Write an annotated burst.toml into the current directory
    Init {
        /// Target GitHub repository as owner/repo
        repo: String,
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

fn main() -> ExitCode {
    // The help text is built before parsing so it can carry the setup steps
    // this directory still needs; a directory with nothing outstanding gets
    // the plain help.
    let cwd = std::env::current_dir();
    let mut command = Cli::command();
    if let Some(setup) = cwd.as_ref().ok().and_then(|d| burst::setup::probe(d)) {
        command = command.after_help(setup);
    }
    let cli = match Cli::from_arg_matches(&command.get_matches()) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };

    let cwd = match cwd {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // `init` is the one command that runs without a config — it writes one.
    if let Cmd::Init { repo } = &cli.command {
        return match burst::commands::init::run(&cwd, repo) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let config = match burst::config::load(&cwd, cli.repo.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cli.command {
        Cmd::Up {
            n,
            auto,
            spot,
            yes,
            ssh_key,
        } => match burst::commands::up::run(
            &config,
            &burst::commands::up::UpArgs {
                n,
                auto,
                spot,
                yes,
                ssh_key,
            },
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        // Handled before the config load above; the match must stay exhaustive.
        Cmd::Init { .. } => ExitCode::SUCCESS,
        Cmd::Bake => match burst::commands::bake::run(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Cmd::Status => match burst::commands::status::run(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Cmd::Down { yes } => match burst::commands::down::run(&config, yes) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Cmd::Sweep => match burst::commands::sweep::run(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
