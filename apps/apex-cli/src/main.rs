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

mod admin;
mod auth;
mod config;
mod kms;
mod memory;
mod plugin;
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

    /// Install and manage plugins (extension capabilities).
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },

    /// Manage the platform KMS's tenant keys (docs/13-security/encryption.md §5).
    Kms {
        #[command(subcommand)]
        command: KmsCommand,
    },

    /// Manage API keys for `APEX_AUTH_MODE=apikey` (RM-GA-P1 SEC-101).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// Backup and restore the local `~/.apex` state directory (RM-GA-P2 DR-1001).
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Snapshot `~/.apex` into `<dest>` (agents, secrets, memory, workflows,
    /// tenancy, kms, and every other local store), quiescing every
    /// DUR-403-locked store directory for a consistent point-in-time copy.
    Backup {
        /// Destination directory to write the backup into (created if missing).
        dest: String,
    },

    /// Restore `~/.apex` from a backup made by `apex admin backup`. Overwrites
    /// the live `~/.apex` — irreversible for anything written there since the
    /// backup was taken.
    Restore {
        /// Source backup directory, as produced by `apex admin backup`.
        src: String,

        /// Confirm the overwrite (required).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Mint a fresh API key that authenticates as `principal`, printed once.
    CreateKey {
        /// The principal the minted key authenticates as.
        principal: String,
    },
}

#[derive(Subcommand)]
enum KmsCommand {
    /// Roll a new tenant-key version. Existing wrapped data keys remain valid under
    /// their original version — nothing already sealed is re-encrypted.
    Rotate {
        /// Tenant to rotate.
        #[arg(long)]
        tenant: String,
    },

    /// Permanently crypto-shred a tenant's key material. IRREVERSIBLE — every
    /// secret/memory ever sealed under this tenant becomes unrecoverable.
    Destroy {
        /// Tenant to destroy.
        #[arg(long)]
        tenant: String,

        /// Confirm the irreversible action (required).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Generate an ed25519 signing keypair for a publisher.
    Keygen {
        /// Publisher name the keypair signs for.
        publisher: String,

        /// Directory to write the `<publisher>.key` / `.pub` files into.
        #[arg(long, default_value = ".")]
        dir: String,
    },

    /// Sign a plugin manifest, producing a detached signature.
    Sign {
        /// PKCS#8 ed25519 private key (from `keygen`).
        #[arg(long)]
        key: String,

        /// Path to the plugin manifest (`plugin.yaml`).
        #[arg(long)]
        manifest: String,

        /// Output signature path (defaults to `plugin.sig` beside the manifest).
        #[arg(long)]
        out: Option<String>,
    },

    /// Set up keyless trust on this node: a dev CA + the pinned trust config
    /// (`~/.apex/plugins/keyless.json`, ADR-0009).
    KeylessInit {
        /// Identity grant as `issuer|subject|publisher` (repeatable; `subject` and
        /// `publisher` accept a trailing `*` wildcard).
        #[arg(long = "allow")]
        allow: Vec<String>,
    },

    /// Keyless-sign a plugin manifest: a short-lived identity certificate over an
    /// ephemeral key that never touches disk (ADR-0009).
    KeylessSign {
        /// Path to the plugin manifest (`plugin.yaml`).
        #[arg(long)]
        manifest: String,

        /// Signer identity issuer (e.g. `https://ci.example.com`).
        #[arg(long)]
        issuer: String,

        /// Signer identity subject (e.g. `release@acme.dev`).
        #[arg(long)]
        subject: String,

        /// Rekor transparency-log URL to witness the signing
        /// (requires a `--features keyless-rekor` build).
        #[arg(long)]
        rekor: Option<String>,

        /// CA key path (defaults to `~/.apex/plugins/keyless-ca.key`).
        #[arg(long)]
        ca_key: Option<String>,
    },

    /// Bundle a package directory into a single distributable `.apexpkg` file.
    Pack {
        /// Path to the plugin package directory.
        dir: String,

        /// Output file (defaults to `<name>-<version>.apexpkg`).
        #[arg(long)]
        out: Option<String>,
    },

    /// Trust a publisher's public key so its packages verify on install.
    Trust {
        /// Publisher name to trust.
        publisher: String,

        /// Path to the publisher's raw ed25519 public key (from `keygen`).
        #[arg(long)]
        key: String,
    },

    /// Install a plugin package directory (`plugin.yaml` + `plugin.sig` + artifacts).
    Install {
        /// Path to the plugin package directory.
        dir: String,

        /// Permission to grant the plugin (repeatable); must cover all it requests.
        #[arg(long = "grant")]
        grants: Vec<String>,
    },

    /// Upgrade an installed plugin to the version in a package directory.
    Upgrade {
        /// Path to the new plugin package directory.
        dir: String,

        /// Permission to grant for the new version (repeatable); covers any new perms.
        #[arg(long = "grant")]
        grants: Vec<String>,
    },

    /// Roll a plugin back to its previous version.
    Rollback {
        /// Plugin id (`publisher/name`).
        id: String,
    },

    /// Invoke an enabled plugin tool capability directly (operator test path).
    Run {
        /// Capability id to invoke (e.g. `echo.run`).
        capability: String,

        /// Request parameters as JSON (plain text also accepted).
        #[arg(long, default_value = "{}")]
        input: String,
    },

    /// List installed plugins.
    List,

    /// Enable an installed plugin's capabilities.
    Enable {
        /// Plugin id (`publisher/name`).
        id: String,
    },

    /// Disable a plugin's capabilities (state retained).
    Disable {
        /// Plugin id (`publisher/name`).
        id: String,
    },

    /// Uninstall a plugin and remove its staged artifacts.
    Uninstall {
        /// Plugin id (`publisher/name`).
        id: String,
    },

    /// Publish a signed package to the marketplace registry.
    Publish {
        /// Package directory or `.apexpkg` file to publish.
        source: String,

        /// Channel to publish to (default `stable`).
        #[arg(long)]
        channel: Option<String>,

        /// Browse category (repeatable).
        #[arg(long = "category")]
        categories: Vec<String>,
    },

    /// Search the marketplace registry for published plugins.
    Search {
        /// Free-text query (matches name/publisher/description/categories).
        query: Option<String>,

        /// Filter by category.
        #[arg(long)]
        category: Option<String>,

        /// Filter by capability kind (tool/provider/memory_backend/policy/workflow_activity).
        #[arg(long)]
        capability: Option<String>,
    },

    /// Download a listed package from the marketplace and install it (disabled).
    Get {
        /// Listing id (`publisher/name`).
        id: String,

        /// Specific version (default: the latest stable).
        #[arg(long)]
        version: Option<String>,

        /// Permission to grant the plugin (repeatable); must cover all it requests.
        #[arg(long = "grant")]
        grants: Vec<String>,
    },

    /// File an abuse report against a marketplace listing (malware, IP infringement,
    /// deceptive metadata, etc.).
    Report {
        /// Listing id (`publisher/name`).
        id: String,

        /// Why the listing is being reported.
        reason: String,

        /// Reporting identity (default `anonymous`).
        #[arg(long)]
        reporter: Option<String>,
    },

    /// List the abuse reports filed against a marketplace listing.
    Reports {
        /// Listing id (`publisher/name`).
        id: String,
    },

    /// Resolve an open abuse report as valid, optionally delisting the listing.
    ResolveAbuse {
        /// Listing id (`publisher/name`).
        id: String,

        /// Report id (0-based, from `plugin reports`).
        report_id: u64,

        /// Remove the listing from discovery/download.
        #[arg(long)]
        delist: bool,

        /// Moderating identity (default `operator`).
        #[arg(long)]
        moderator: Option<String>,
    },

    /// Dismiss an open abuse report as not actionable.
    DismissAbuse {
        /// Listing id (`publisher/name`).
        id: String,

        /// Report id (0-based, from `plugin reports`).
        report_id: u64,

        /// Why the report was found not actionable.
        reason: String,

        /// Moderating identity (default `operator`).
        #[arg(long)]
        moderator: Option<String>,
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

        /// Tenant the run acts in — scopes any plugin-tool secret resolution to this
        /// tenant's vault namespace (local mode; requires the `plugin-wasi` build).
        #[arg(long)]
        tenant: Option<String>,

        /// Override the model/tool iteration cap (default: 8). Raise this for tasks
        /// that need many tool calls to finish; a run that hits the cap without a
        /// final answer errors with "did not finish within N steps".
        #[arg(long = "max-steps")]
        max_steps: Option<usize>,

        /// Provider backend for a local run (local mode only). `auto` (default)
        /// mirrors `Gateway::from_env()` — OpenAI if `OPENAI_API_KEY` is set, else the
        /// deterministic mock. `mistralrs` runs a real local model in-process via
        /// mistral.rs (needs a `--features mistralrs` build; first use downloads GGUF
        /// weights — see APEX_MISTRALRS_GGUF_REPO/_GGUF_FILE/_TOK_MODEL_ID).
        #[arg(long, default_value = "auto")]
        provider: String,
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

        /// Directory to resolve `agent`-typed activities' `name` against as
        /// `<agents-dir>/<name>.yaml` (defaults to the current directory). The
        /// server instead resolves `name` against a *stored* agent id — this is
        /// the CLI's file-based equivalent for local dev.
        #[arg(long, default_value = ".")]
        agents_dir: String,
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

    /// Show an execution's live state (a side-effect-free query).
    Status {
        /// Execution id reported by `workflows run`.
        #[arg(long)]
        id: String,
    },

    /// List executions, optionally filtered by workflow/status.
    List {
        /// Only executions of this workflow name.
        #[arg(long)]
        workflow: Option<String>,

        /// Only executions in this status (e.g. running, completed, failed).
        #[arg(long)]
        status: Option<String>,

        /// Cap the number of executions listed.
        #[arg(long)]
        limit: Option<usize>,
    },

    /// Show an execution's status and full event timeline.
    Show {
        /// Execution id reported by `workflows run`.
        #[arg(long)]
        id: String,
    },

    /// Fire due wall-clock timers and start due schedules for a workflow.
    Tick {
        /// Path to the workflow YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,
    },

    /// Manage recurring schedules.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
}

#[derive(Subcommand)]
enum ScheduleCommand {
    /// Register a recurring schedule that starts a workflow on an interval.
    Create {
        /// Path to the workflow YAML definition.
        #[arg(short = 'f', long = "file")]
        file: String,

        /// Unique schedule id (also the execution-id prefix).
        #[arg(long)]
        id: String,

        /// Interval between runs, in milliseconds (mutually exclusive with --cron).
        #[arg(long, conflicts_with = "cron")]
        every: Option<u64>,

        /// Cron expression (5-field or @macro, UTC; mutually exclusive with --every).
        #[arg(long)]
        cron: Option<String>,

        /// Run input as JSON passed to each execution.
        #[arg(long, default_value = "{}")]
        input: String,
    },

    /// List registered schedules.
    List,
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

        /// Seal the content at rest through the platform KMS
        /// (docs/13-security/encryption.md §4).
        #[arg(long)]
        sensitive: bool,
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

        /// Retrieval strategy: hybrid (default), vector, or keyword. Use `keyword`
        /// offline — the mock embeddings make hybrid/vector noisy.
        #[arg(long)]
        strategy: Option<String>,

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
                tenant,
                max_steps,
                provider,
            } => {
                run_agent_cmd(
                    &file, &input, local, server, stream, tenant, max_steps, &provider,
                )
                .await
            }
        },
        Command::Workflows { command } => match command {
            WorkflowsCommand::Validate { file } => workflow::validate_cmd(&file),
            WorkflowsCommand::Run {
                file,
                input,
                local,
                id,
                agents_dir,
            } => workflow::run_cmd(&file, &input, local, id, &agents_dir).await,
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
            WorkflowsCommand::Status { id } => workflow::status_cmd(&id).await,
            WorkflowsCommand::List {
                workflow,
                status,
                limit,
            } => workflow::list_cmd(workflow, status, limit).await,
            WorkflowsCommand::Show { id } => workflow::show_cmd(&id).await,
            WorkflowsCommand::Tick { file } => workflow::tick_cmd(&file).await,
            WorkflowsCommand::Schedule { command } => match command {
                ScheduleCommand::Create {
                    file,
                    id,
                    every,
                    cron,
                    input,
                } => workflow::schedule_create_cmd(&file, &id, every, cron, &input).await,
                ScheduleCommand::List => workflow::schedule_list_cmd().await,
            },
        },
        Command::Memory { command } => match command {
            MemoryCommand::Put {
                namespace,
                content,
                importance,
                tags,
                require_scopes,
                sensitive,
            } => {
                memory::put_cmd(
                    &namespace,
                    &content,
                    importance,
                    tags,
                    require_scopes,
                    sensitive,
                )
                .await
            }
            MemoryCommand::Query {
                query,
                namespace,
                limit,
                diversity,
                strategy,
                grants,
            } => memory::query_cmd(&query, namespace, limit, diversity, strategy, grants).await,
            MemoryCommand::Compact {
                namespace,
                max_importance,
                keep_recent,
            } => memory::compact_cmd(&namespace, max_importance, keep_recent).await,
        },
        Command::Plugin { command } => match command {
            PluginCommand::Keygen { publisher, dir } => plugin::keygen_cmd(&publisher, &dir),
            PluginCommand::Sign { key, manifest, out } => plugin::sign_cmd(&key, &manifest, out),
            PluginCommand::KeylessInit { allow } => plugin::keyless_init_cmd(allow),
            PluginCommand::KeylessSign {
                manifest,
                issuer,
                subject,
                rekor,
                ca_key,
            } => plugin::keyless_sign_cmd(&manifest, &issuer, &subject, rekor, ca_key),
            PluginCommand::Pack { dir, out } => plugin::pack_cmd(&dir, out),
            PluginCommand::Trust { publisher, key } => plugin::trust_cmd(&publisher, &key),
            PluginCommand::Install { dir, grants } => plugin::install_cmd(&dir, grants),
            PluginCommand::Upgrade { dir, grants } => plugin::upgrade_cmd(&dir, grants),
            PluginCommand::Rollback { id } => plugin::rollback_cmd(&id),
            PluginCommand::Run { capability, input } => plugin::run_cmd(&capability, &input).await,
            PluginCommand::List => plugin::list_cmd(),
            PluginCommand::Enable { id } => plugin::enable_cmd(&id),
            PluginCommand::Disable { id } => plugin::disable_cmd(&id),
            PluginCommand::Uninstall { id } => plugin::uninstall_cmd(&id),
            PluginCommand::Publish {
                source,
                channel,
                categories,
            } => plugin::publish_cmd(&source, channel, categories),
            PluginCommand::Search {
                query,
                category,
                capability,
            } => plugin::search_cmd(query, category, capability),
            PluginCommand::Get {
                id,
                version,
                grants,
            } => plugin::market_install_cmd(&id, version, grants),
            PluginCommand::Report {
                id,
                reason,
                reporter,
            } => plugin::report_abuse_cmd(&id, &reason, reporter),
            PluginCommand::Reports { id } => plugin::list_abuse_reports_cmd(&id),
            PluginCommand::ResolveAbuse {
                id,
                report_id,
                delist,
                moderator,
            } => plugin::resolve_abuse_cmd(&id, report_id, delist, moderator),
            PluginCommand::DismissAbuse {
                id,
                report_id,
                reason,
                moderator,
            } => plugin::dismiss_abuse_cmd(&id, report_id, &reason, moderator),
        },
        Command::Kms { command } => match command {
            KmsCommand::Rotate { tenant } => kms::rotate_cmd(&tenant),
            KmsCommand::Destroy { tenant, yes } => kms::destroy_cmd(&tenant, yes),
        },
        Command::Auth { command } => match command {
            AuthCommand::CreateKey { principal } => auth::create_key_cmd(&principal),
        },
        Command::Admin { command } => match command {
            AdminCommand::Backup { dest } => admin::backup_cmd(&dest),
            AdminCommand::Restore { src, yes } => admin::restore_cmd(&src, yes),
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

// Pre-existing CLI arg surface (one flag per `agents run` option); a builder/options
// struct is a larger refactor than this unrelated lint warrants on its own.
#[allow(clippy::too_many_arguments)]
async fn run_agent_cmd(
    file: &str,
    input: &str,
    local: bool,
    server: Option<String>,
    stream: bool,
    tenant: Option<String>,
    max_steps: Option<usize>,
    provider: &str,
) -> apex_common::Result<()> {
    // Accept JSON input, or fall back to treating the argument as plain text.
    let input_value: Value =
        serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()));

    if local {
        return run_local(file, input_value, stream, tenant, max_steps, provider).await;
    }
    if provider != "auto" {
        eprintln!("note: --provider is local-only; ignoring for a remote run");
    }
    run_remote(file, input_value, server, stream, max_steps).await
}

/// Build the gateway a local run uses. `auto` mirrors `Gateway::from_env()`; other
/// names select an explicit backend (currently only `mistralrs`, gated behind the
/// `mistralrs` cargo feature since it pulls a heavy inference engine).
async fn build_local_gateway(provider: &str) -> apex_common::Result<Gateway> {
    match provider {
        "auto" => Ok(Gateway::from_env()),
        "mistralrs" => {
            #[cfg(feature = "mistralrs")]
            {
                let backend = apex_provider::MistralRsProvider::from_env().await?;
                Ok(Gateway::new(Box::new(backend)))
            }
            #[cfg(not(feature = "mistralrs"))]
            {
                Err(apex_common::Error::config(
                    "provider \"mistralrs\" needs a build with --features mistralrs (this \
                     binary was built without it)",
                ))
            }
        }
        other => Err(apex_common::Error::config(format!(
            "unknown provider \"{other}\" (expected \"auto\" or \"mistralrs\")"
        ))),
    }
}

/// Run the agent in-process with the embedded runtime.
async fn run_local(
    file: &str,
    input: Value,
    stream: bool,
    tenant: Option<String>,
    max_steps: Option<usize>,
    provider: &str,
) -> apex_common::Result<()> {
    let def = AgentDefinition::from_file(file)?;
    let gateway = build_local_gateway(provider).await?;
    // `agents run --local` is a trusted, first-party/local context (SEC-301's
    // documented escape hatch) — shell stays available here, unlike the server's
    // default registry.
    let mut registry = ToolRegistry::with_privileged_builtins();
    // image_generate needs a real, billed API key, so it's only registered when one is
    // configured — same signal build_local_gateway/Gateway::from_env use to pick a real
    // vs. mock provider.
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        registry.register(std::sync::Arc::new(apex_tools::ImageGenTool::new()));
    }
    // Make enabled plugins' tool capabilities callable by the agent.
    plugin::engine()?.register_enabled(&mut registry);
    let mut opts = RunOptions::new(input);
    if let Some(t) = tenant {
        opts = opts.with_tenant(t);
    }
    // An explicit --max-steps wins; otherwise fall back to the agent's own default.
    if let Some(n) = max_steps.or(def.spec.max_steps) {
        opts = opts.with_max_steps(n);
    }

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
    max_steps: Option<usize>,
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

    let mut body = serde_json::json!({ "manifest": manifest, "input": input });
    if let Some(n) = max_steps {
        body["max_steps"] = serde_json::json!(n);
    }
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
