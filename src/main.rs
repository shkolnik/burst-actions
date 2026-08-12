use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "burst",
    version,
    about = "Ephemeral cloud VMs as GitHub Actions runners",
    long_about = "Ephemeral cloud VMs as GitHub Actions runners.\n\n                  Launches EC2 instances that each run exactly one queued job and terminate. \
                  Needs AWS credentials from the usual chain (env vars, profile, SSO) and a \
                  GitHub PAT; jobs reach these runners with `runs-on: [self-hosted, burst]`."
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
        /// Size the fleet from queued jobs labeled `burst`
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
    /// Build the runner AMI now; `burst up` builds it on demand anyway
    Bake,
    /// Show live fleet state, read from AWS rather than the local statefile
    Status,
    /// Terminate this repo's fleet
    Down {
        /// Skip the confirmation prompt (for automation)
        #[arg(long)]
        yes: bool,
    },
    /// Reap expired instances, orphaned kill schedules, stale runner registrations
    Sweep,
}

fn main() -> ExitCode {
    // The help text is built before parsing so it can carry the setup steps
    // this directory still needs; a directory with nothing outstanding gets
    // the plain help.
    let cwd = std::env::current_dir();
    // `--help` also states the prerequisites the Setup block cannot probe for
    // (AWS credentials) and the one thing that must change in the consuming
    // repo's workflow. The label comes from the same constant burst mints.
    let mut command = Cli::command().long_about(format!(
        "Ephemeral cloud VMs as GitHub Actions runners.\n\n\
         Launches EC2 instances that each run exactly one queued job and terminate. Needs AWS \
         credentials from the usual chain (env vars, profile, SSO) and a GitHub PAT. Jobs reach \
         these runners with `{}` in the workflow.",
        burst::github::RUNS_ON
    ));
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
