//! `apex` — the Apex AI Platform command-line interface.
//!
//! v0.1 implements the local agent run path from the
//! [CLI reference §10](../../docs/11-cli/commands.md) and the
//! [hello agent](../../docs/16-examples/hello-agent.md) example:
//!
//! ```bash
//! apex agents run --local -f agents/hello.yaml --input '{"message":"Hi"}' --stream
//! ```
//!
//! Remote (server) execution is stubbed and returns a clear "not yet implemented"
//! message; it arrives with the API/server work in a later milestone.

mod stream;

use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent};
use apex_provider::Gateway;
use apex_tools::ToolRegistry;
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::process::ExitCode;
use stream::StreamSink;

/// Top-level CLI.
#[derive(Parser)]
#[command(name = "apex", version, about = "Apex AI Platform CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage and run agents.
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// Run an agent.
    Run {
        /// Path to the agent YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,

        /// Run input as JSON (e.g. '{"message":"Hi"}'). Plain text is also accepted.
        #[arg(long, default_value = "{}")]
        input: String,

        /// Use the embedded local runtime (the only supported mode in v0.1).
        #[arg(long)]
        local: bool,

        /// Render the run as a live event stream.
        #[arg(long)]
        stream: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // Logs go to stderr; default to warn so normal runs have clean stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> apex_common::Result<()> {
    match cli.command {
        Command::Agents { command } => match command {
            AgentsCommand::Run {
                file,
                input,
                local,
                stream,
            } => run_agent_cmd(&file, &input, local, stream).await,
        },
    }
}

async fn run_agent_cmd(
    file: &str,
    input: &str,
    local: bool,
    stream: bool,
) -> apex_common::Result<()> {
    if !local {
        return Err(apex_common::Error::config(
            "v0.1 supports local runs only; pass --local (remote execution lands with the server milestone)",
        ));
    }

    let def = AgentDefinition::from_file(file)?;

    // Accept JSON input, or fall back to treating the argument as plain text.
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));

    let gateway = Gateway::from_env();
    let registry = ToolRegistry::with_builtins();
    let opts = RunOptions::new(input_value);

    if stream {
        let mut sink = StreamSink::new();
        let out = run_agent(&def, &gateway, &registry, opts, &mut sink).await?;
        // In stream mode the `done` line already reported usage; nothing more.
        let _ = out;
    } else {
        let out = run_agent(&def, &gateway, &registry, opts, &mut NullSink).await?;
        println!("{}", out.text);
        eprintln!(
            "usage: tokens={}, cost_usd={:.6}",
            out.usage.total_tokens, out.usage.cost_usd
        );
    }

    Ok(())
}
