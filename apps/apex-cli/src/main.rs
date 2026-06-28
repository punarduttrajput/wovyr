//! `apex` — the Apex AI Platform command-line interface.
//!
//! v0.1 implements the local agent run path plus basic auth from the
//! [CLI reference](../../docs/11-cli/commands.md) and the
//! [hello agent](../../docs/16-examples/hello-agent.md) example:
//!
//! ```bash
//! apex login --server https://api.apex.example.com --token <token>
//! apex agents run --local -f agents/hello.yaml --input '{"message":"Hi"}' --stream
//! apex dev                                  # start a single-node server
//! apex agents run -f agents/hello.yaml --input '{"message":"Hi"}'  # remote run
//! ```
//!
//! Local (`--local`) and remote runs are both supported. Remote streaming (SSE)
//! and a persistent agent store arrive in a later milestone.

mod config;
mod memory;
mod stream;
mod workflow;

use apex_agent::{AgentDefinition, NullSink, RunOptions, run_agent, run_agent_with_memory};
use apex_provider::Gateway;
use apex_tools::ToolRegistry;
use clap::{Parser, Subcommand};
use config::Credentials;
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
    /// Authenticate against a server and store credentials locally.
    Login {
        /// Server base URL (e.g. https://api.apex.example.com).
        #[arg(long)]
        server: String,

        /// Access token. Falls back to the APEX_TOKEN environment variable.
        #[arg(long)]
        token: Option<String>,
    },

    /// Remove stored credentials.
    Logout,

    /// Show the current local identity (server + masked token).
    Whoami,

    /// Run an all-in-one local platform server for testing.
    Dev {
        /// Address to bind (host:port).
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
    },

    /// Manage and run agents.
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },

    /// Validate and run workflows.
    Workflows {
        #[command(subcommand)]
        command: WorkflowsCommand,
    },

    /// Store and query agent memory.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
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

        /// Use the embedded local runtime instead of a server.
        #[arg(long)]
        local: bool,

        /// Server base URL for a remote run. Falls back to stored credentials.
        #[arg(long)]
        server: Option<String>,

        /// Render the run as a live event stream (local mode only in v0.1).
        #[arg(long)]
        stream: bool,
    },
}

#[derive(Subcommand)]
enum WorkflowsCommand {
    /// Compile-check a workflow definition.
    Validate {
        /// Path to the workflow YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,
    },

    /// Run a workflow with the embedded engine.
    Run {
        /// Path to the workflow YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,

        /// Run input as JSON. Plain text is also accepted.
        #[arg(long, default_value = "{}")]
        input: String,

        /// Use the embedded local runtime (the only supported mode in v0.2).
        #[arg(long)]
        local: bool,

        /// Execution id (defaults to `wf-<workflow-name>`). Use to resume/approve.
        #[arg(long)]
        id: Option<String>,
    },

    /// Approve a suspended human task and resume the execution.
    Approve {
        /// Path to the workflow YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,

        /// Execution id reported by `workflows run`.
        #[arg(long)]
        id: String,

        /// The human activity id to decide.
        #[arg(long)]
        task: String,

        /// The decision to record (e.g. approved / rejected).
        #[arg(long, default_value = "approved")]
        decision: String,
    },

    /// Deliver a timer/event signal to a waiting execution and resume it.
    Signal {
        /// Path to the workflow YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,

        /// Execution id reported by `workflows run`.
        #[arg(long)]
        id: String,

        /// Name of the event to deliver (mutually exclusive with --timer).
        #[arg(long, conflicts_with = "timer")]
        event: Option<String>,

        /// Id of the timer to fire (mutually exclusive with --event).
        #[arg(long)]
        timer: Option<String>,

        /// JSON payload for an event (default null).
        #[arg(long, default_value = "null")]
        payload: String,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Store a memory.
    Put {
        /// Namespace to store under.
        #[arg(long, default_value = "default")]
        namespace: String,

        /// The memory text.
        #[arg(long)]
        content: String,

        /// Intrinsic importance in [0,1].
        #[arg(long, default_value_t = 0.5)]
        importance: f32,

        /// Tags for metadata filtering (repeatable).
        #[arg(long = "tag")]
        tags: Vec<String>,

        /// Access scope a reader must be granted to retrieve this memory (repeatable).
        #[arg(long = "require-scope")]
        require_scopes: Vec<String>,
    },

    /// Query memories by relevance.
    Query {
        /// Query text.
        query: String,

        /// Restrict to a namespace.
        #[arg(long)]
        namespace: Option<String>,

        /// Maximum results.
        #[arg(long, default_value_t = 5)]
        limit: usize,

        /// Result diversification via MMR in [0,1] (0 = pure relevance).
        #[arg(long, default_value_t = 0.0)]
        diversity: f32,

        /// Access scope the reader holds, for ABAC filtering (repeatable).
        #[arg(long = "grant")]
        grants: Vec<String>,
    },

    /// Consolidate stale, low-importance memories into a summary.
    Compact {
        /// Namespace to compact.
        #[arg(long, default_value = "default")]
        namespace: String,

        /// Only consolidate records with importance below this.
        #[arg(long, default_value_t = 0.5)]
        max_importance: f32,

        /// Keep the most recent N records untouched.
        #[arg(long, default_value_t = 5)]
        keep_recent: usize,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // Structured, leveled logging to stderr (APEX_LOG / APEX_LOG_FORMAT=json), plus
    // OTLP trace export when built with `--features otlp` and the endpoint is set.
    // Held until `main` returns so batched spans flush on exit.
    let _telemetry = apex_telemetry::init_logging();

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
        Command::Login { server, token } => login_cmd(server, token),
        Command::Logout => logout_cmd(),
        Command::Whoami => whoami_cmd(),
        Command::Dev { addr } => dev_cmd(&addr).await,
        Command::Agents { command } => match command {
            AgentsCommand::Run {
                file,
                input,
                local,
                server,
                stream,
            } => run_agent_cmd(&file, &input, local, server, stream).await,
        },
        Command::Workflows { command } => match command {
            WorkflowsCommand::Validate { file } => workflow::validate_cmd(&file),
            WorkflowsCommand::Run {
                file,
                input,
                local,
                id,
            } => workflow::run_cmd(&file, &input, local, id).await,
            WorkflowsCommand::Approve {
                file,
                id,
                task,
                decision,
            } => workflow::approve_cmd(&file, &id, &task, &decision).await,
            WorkflowsCommand::Signal {
                file,
                id,
                event,
                timer,
                payload,
            } => workflow::signal_cmd(&file, &id, event, timer, &payload).await,
        },
        Command::Memory { command } => match command {
            MemoryCommand::Put {
                namespace,
                content,
                importance,
                tags,
                require_scopes,
            } => memory::put_cmd(&namespace, &content, importance, tags, require_scopes).await,
            MemoryCommand::Query {
                query,
                namespace,
                limit,
                diversity,
                grants,
            } => memory::query_cmd(&query, namespace, limit, diversity, grants).await,
            MemoryCommand::Compact {
                namespace,
                max_importance,
                keep_recent,
            } => memory::compact_cmd(&namespace, max_importance, keep_recent).await,
        },
    }
}

async fn dev_cmd(addr: &str) -> apex_common::Result<()> {
    let socket = addr
        .parse()
        .map_err(|e| apex_common::Error::config(format!("invalid --addr `{addr}`: {e}")))?;
    println!("Starting Apex dev server on http://{addr} (Ctrl-C to stop)");
    apex_server::serve(socket).await
}

fn login_cmd(server: String, token: Option<String>) -> apex_common::Result<()> {
    let token = token
        .or_else(|| std::env::var("APEX_TOKEN").ok())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            apex_common::Error::config("no token provided; pass --token or set APEX_TOKEN")
        })?;

    let creds = Credentials { server, token };
    config::save_credentials(&creds)?;
    // Never print the raw token.
    println!(
        "Logged in to {} (token {})",
        creds.server,
        creds.masked_token()
    );
    Ok(())
}

fn logout_cmd() -> apex_common::Result<()> {
    if config::delete_credentials()? {
        println!("Logged out.");
    } else {
        println!("Not logged in.");
    }
    Ok(())
}

fn whoami_cmd() -> apex_common::Result<()> {
    match config::load_credentials()? {
        Some(creds) => println!("{} (token {})", creds.server, creds.masked_token()),
        None => println!("Not logged in. Run `apex login --server <url> --token <token>`."),
    }
    Ok(())
}

async fn run_agent_cmd(
    file: &str,
    input: &str,
    local: bool,
    server: Option<String>,
    stream: bool,
) -> apex_common::Result<()> {
    // Accept JSON input, or fall back to treating the argument as plain text.
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));

    if local {
        return run_local(file, input_value, stream).await;
    }
    run_remote(file, input_value, server, stream).await
}

/// Run the agent in-process with the embedded runtime.
async fn run_local(file: &str, input: Value, stream: bool) -> apex_common::Result<()> {
    let def = AgentDefinition::from_file(file)?;
    let gateway = Gateway::from_env();
    let registry = ToolRegistry::with_builtins();
    let opts = RunOptions::new(input);

    // Open a memory retriever only when the agent enables it (RAG agents).
    let retriever = match &def.spec.memory {
        Some(m) if m.enabled => Some(memory::EngineRetriever::open().await?),
        _ => None,
    };

    if stream {
        let mut sink = StreamSink::new();
        match &retriever {
            Some(r) => run_agent_with_memory(&def, &gateway, &registry, opts, r, &mut sink).await?,
            None => run_agent(&def, &gateway, &registry, opts, &mut sink).await?,
        };
    } else {
        let out = match &retriever {
            Some(r) => {
                run_agent_with_memory(&def, &gateway, &registry, opts, r, &mut NullSink).await?
            }
            None => run_agent(&def, &gateway, &registry, opts, &mut NullSink).await?,
        };
        println!("{}", out.text);
        eprintln!(
            "usage: tokens={}, cost_usd={:.6}",
            out.usage.total_tokens, out.usage.cost_usd
        );
    }
    Ok(())
}

/// Run the agent against a single-node server via the Agents API.
async fn run_remote(
    file: &str,
    input: Value,
    server: Option<String>,
    stream: bool,
) -> apex_common::Result<()> {
    if stream {
        eprintln!("note: --stream is local-only in v0.1; performing a non-streaming remote run");
    }

    // Resolve the server URL: explicit flag wins, else stored credentials.
    let creds = config::load_credentials()?;
    let base = server
        .or_else(|| creds.as_ref().map(|c| c.server.clone()))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            apex_common::Error::config(
                "no server configured; pass --server <url>, run `apex login`, or use --local",
            )
        })?;
    let base = base.trim_end_matches('/');

    let manifest = std::fs::read_to_string(file).map_err(|e| {
        apex_common::Error::config(format!("could not read agent file {file}: {e}"))
    })?;

    let body = serde_json::json!({ "manifest": manifest, "input": input });
    let url = format!("{base}/api/v1/agents:run");

    let mut request = reqwest::Client::new().post(&url).json(&body);
    if let Some(c) = &creds {
        request = request.bearer_auth(&c.token);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| apex_common::Error::provider(format!("request to {url} failed: {e}")))?;

    let status = resp.status();
    let payload: Value = resp
        .json()
        .await
        .map_err(|e| apex_common::Error::provider(format!("decoding response failed: {e}")))?;

    if !status.is_success() {
        let msg = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(apex_common::Error::provider(format!(
            "server returned {status}: {msg}"
        )));
    }

    if let Some(message) = payload.pointer("/output/message").and_then(Value::as_str) {
        println!("{message}");
    }
    let tokens = payload
        .pointer("/usage/total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cost = payload
        .pointer("/usage/cost_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let run_id = payload.get("run_id").and_then(Value::as_str).unwrap_or("");
    eprintln!("run: {run_id}, tokens={tokens}, cost_usd={cost:.6}");
    Ok(())
}
