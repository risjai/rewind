mod share;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use rewind_assert::{AssertionEngine, BaselineManager, Tolerance};
use rewind_eval::{DatasetManager, EvaluatorRegistry, ExperimentRunner, RunConfig, extract_timeline_output};
use rewind_proxy::ProxyServer;
use rewind_replay::ReplayEngine;
use rewind_store::{Store, Timeline};
use rewind_tui::TuiApp;
use rewind_web::WebServer;
use std::net::SocketAddr;

#[derive(Parser)]
#[command(
    name = "rewind",
    about = "⏪ Rewind — Chrome DevTools for AI agents",
    long_about = "Time-travel debugger for AI agents. Record, inspect, fork, replay, diff.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start recording agent LLM calls via local proxy
    Record {
        /// Session name
        #[arg(short, long, default_value = "default")]
        name: String,

        /// Proxy listen port
        #[arg(short, long, default_value = "8443")]
        port: u16,

        /// Upstream LLM API base URL
        #[arg(short, long, default_value = "https://api.openai.com")]
        upstream: String,

        /// Enable Instant Replay — cache responses and serve from cache on identical requests
        #[arg(long)]
        replay: bool,

        /// Also start the web dashboard for live observability
        #[arg(long)]
        web: bool,

        /// Port for the web dashboard (used with --web)
        #[arg(long, default_value = "8080")]
        web_port: u16,

        /// Skip TLS certificate verification for upstream connections (INSECURE)
        #[arg(long)]
        insecure: bool,
    },

    /// List recorded sessions
    Sessions,

    /// Inspect a session in the TUI
    Inspect {
        /// Session ID or "latest"
        #[arg(default_value = "latest")]
        session: String,
    },

    /// Show steps of a session (non-interactive)
    Show {
        /// Session ID or "latest"
        #[arg(default_value = "latest")]
        session: String,

        /// Show flat step list instead of span tree
        #[arg(long)]
        flat: bool,
    },

    /// List conversation threads (multi-session groupings)
    Threads,

    /// Show a specific conversation thread
    Thread {
        /// Thread ID
        thread_id: String,
    },

    /// Replay a session from a fork point — cached steps served instantly, live from fork point onward
    Replay {
        /// Session ID or "latest"
        #[arg(default_value = "latest")]
        session: String,

        /// Step number to replay from (steps before this are served from cache)
        #[arg(long)]
        from: u32,

        /// Upstream LLM API base URL
        #[arg(short, long, default_value = "https://api.openai.com")]
        upstream: String,

        /// Proxy listen port
        #[arg(short, long, default_value = "8443")]
        port: u16,

        /// Label for the forked timeline
        #[arg(short, long, default_value = "replayed")]
        label: String,
    },

    /// Fork a timeline at a specific step
    Fork {
        /// Session ID or "latest"
        session: String,

        /// Step number to fork at
        #[arg(long)]
        at: u32,

        /// Label for the fork
        #[arg(short, long, default_value = "fork")]
        label: String,
    },

    /// Diff two timelines within a session
    Diff {
        /// Session ID
        session: String,

        /// Left timeline ID
        left: String,

        /// Right timeline ID
        right: String,
    },

    /// Capture a workspace snapshot (files on disk)
    Snapshot {
        /// Directory to snapshot
        #[arg(default_value = ".")]
        directory: String,

        /// Label for this snapshot
        #[arg(short, long, default_value = "checkpoint")]
        label: String,
    },

    /// Restore a workspace from a snapshot
    Restore {
        /// Snapshot ID or label
        snapshot: String,
    },

    /// Share a session as a self-contained HTML file
    Share {
        /// Session ID or "latest"
        session: String,

        /// Include full request/response content (not just metadata)
        #[arg(long)]
        include_content: bool,

        /// Output file path (default: rewind-session-{id}.html)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,

        /// Skip interactive confirmation for --include-content
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// List snapshots
    Snapshots,

    /// Show Instant Replay cache statistics
    Cache,

    /// Seed demo data to showcase the tool
    Demo,

    /// Regression testing — create baselines and check sessions against them
    Assert {
        #[command(subcommand)]
        action: AssertAction,
    },

    /// Start the web dashboard (flight recorder + air traffic control)
    Web {
        /// Port for the web server
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },

    /// Evaluation system — datasets, evaluators, experiments, comparisons
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },

    /// Run a SQL query against the Rewind database (read-only)
    ///
    /// Examples:
    ///   rewind query "SELECT * FROM sessions"
    ///   rewind query "SELECT model, COUNT(*) as calls, SUM(tokens_in + tokens_out) as tokens FROM steps GROUP BY model"
    ///   rewind query --tables
    Query {
        /// SQL query to execute (SELECT only)
        sql: Option<String>,

        /// Show available tables and their schemas
        #[arg(long)]
        tables: bool,
    },

    /// Manage Claude Code hooks integration
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },

    /// Export recorded sessions to external systems
    #[command(name = "export")]
    Export {
        #[command(subcommand)]
        action: ExportAction,
    },

    /// Import traces from external systems into Rewind
    #[command(name = "import")]
    Import {
        #[command(subcommand)]
        action: ImportAction,
    },

    /// Diagnose a failed session and suggest a fix
    Fix {
        /// Session ID, prefix, or 'latest'
        session: String,

        /// Model for the diagnosis LLM call
        #[arg(long, default_value = "gpt-4o-mini")]
        diagnosis_model: String,

        /// Apply the fix: fork, start proxy with rewrites, and optionally score
        #[arg(long)]
        apply: bool,

        /// Agent command to re-run against the patched proxy (requires --apply)
        #[arg(long, short)]
        command: Option<String>,

        /// Upstream LLM base URL for replay
        #[arg(long, default_value = "https://api.openai.com")]
        upstream: String,

        /// Proxy port for replay
        #[arg(long, default_value_t = 8443)]
        port: u16,

        /// Analyze a specific step instead of auto-detecting the error step
        #[arg(long)]
        step: Option<u32>,

        /// Describe expected behavior (for soft failures with no error step)
        #[arg(long)]
        expected: Option<String>,

        /// Skip diagnosis — directly test a fix hypothesis (e.g., 'swap_model:gpt-4o')
        #[arg(long)]
        hypothesis: Option<String>,

        /// Skip confirmation prompts
        #[arg(long)]
        yes: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ImportAction {
    /// Import traces from an OTLP file (protobuf or JSON)
    Otel(OtelImportArgs),
    /// Import a trace from Langfuse by trace ID
    FromLangfuse(LangfuseImportArgs),
}

#[derive(clap::Args)]
struct LangfuseImportArgs {
    /// Langfuse trace ID
    #[arg(long)]
    trace: String,

    /// Langfuse public key (or LANGFUSE_PUBLIC_KEY env)
    #[arg(long, env = "LANGFUSE_PUBLIC_KEY")]
    public_key: Option<String>,

    /// Langfuse secret key (or LANGFUSE_SECRET_KEY env)
    #[arg(long, env = "LANGFUSE_SECRET_KEY")]
    secret_key: Option<String>,

    /// Langfuse host URL
    #[arg(long, env = "LANGFUSE_HOST", default_value = "https://cloud.langfuse.com")]
    host: String,

    /// Override the session name in Rewind
    #[arg(long)]
    name: Option<String>,
}

#[derive(clap::Args)]
struct OtelImportArgs {
    /// Import from a protobuf file (ExportTraceServiceRequest)
    #[arg(long, group = "input", required_unless_present = "json_file")]
    file: Option<std::path::PathBuf>,

    /// Import from a JSON file (OTLP JSON format)
    #[arg(long, group = "input", required_unless_present = "file")]
    json_file: Option<std::path::PathBuf>,

    /// Override the session name
    #[arg(long)]
    name: Option<String>,
}

#[derive(Subcommand)]
enum ExportAction {
    /// Export a session as OpenTelemetry traces via OTLP
    Otel(OtelExportArgs),
}

#[derive(clap::Args)]
struct OtelExportArgs {
    /// Session ID or "latest"
    session: String,

    /// OTLP endpoint URL
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT", default_value = "http://localhost:4318")]
    endpoint: String,

    /// Export protocol: "http" or "grpc"
    #[arg(long, env = "OTEL_EXPORTER_OTLP_PROTOCOL", default_value = "http")]
    protocol: String,

    /// HTTP headers as KEY=VALUE (repeatable, or comma-separated via env var)
    #[arg(long = "header", env = "OTEL_EXPORTER_OTLP_HEADERS", value_delimiter = ',')]
    headers: Vec<String>,

    /// Export a specific timeline (default: main timeline)
    #[arg(long)]
    timeline: Option<String>,

    /// Export all timelines
    #[arg(long)]
    all_timelines: bool,

    /// Include full request/response message content (privacy-sensitive)
    #[arg(long)]
    include_content: bool,

    /// Print spans to stdout instead of sending to an endpoint
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum AssertAction {
    /// Create a baseline from a recorded session
    Baseline {
        /// Session ID, prefix, or "latest"
        #[arg(default_value = "latest")]
        session: String,

        /// Unique name for this baseline
        #[arg(short, long)]
        name: String,

        /// Optional description
        #[arg(short, long, default_value = "")]
        description: String,
    },

    /// Check a session against a baseline for regressions
    Check {
        /// Session ID, prefix, or "latest"
        #[arg(default_value = "latest")]
        session: String,

        /// Baseline name to check against
        #[arg(long)]
        against: String,

        /// Token tolerance percentage (default: 20)
        #[arg(long, default_value = "20")]
        token_tolerance: u32,

        /// Treat model changes as warnings instead of failures
        #[arg(long)]
        warn_model_change: bool,
    },

    /// List all baselines
    List,

    /// Show baseline details
    Show {
        /// Baseline name
        name: String,
    },

    /// Delete a baseline
    Delete {
        /// Baseline name
        name: String,
    },
}

#[derive(Subcommand)]
enum EvalAction {
    /// Manage evaluation datasets
    Dataset {
        #[command(subcommand)]
        action: DatasetAction,
    },

    /// Manage evaluators (scoring functions)
    Evaluator {
        #[command(subcommand)]
        action: EvaluatorAction,
    },

    /// Run an experiment: execute target command against a dataset and score results
    Run {
        /// Dataset name (or name@version)
        dataset: String,

        /// Command to run as the application under test
        #[arg(short, long)]
        command: String,

        /// Evaluator names (can specify multiple: -e exact_match -e contains)
        #[arg(short, long)]
        evaluator: Vec<String>,

        /// Experiment name (auto-generated if omitted)
        #[arg(short, long)]
        name: Option<String>,

        /// Fail if average score drops below this threshold (0.0-1.0)
        #[arg(long)]
        fail_below: Option<f64>,

        /// Timeout per example in seconds
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// Output results as JSON (for CI pipelines)
        #[arg(long)]
        json: bool,

        /// Metadata tags as JSON (e.g., '{"branch":"main","category":"booking"}')
        #[arg(long, default_value = "{}")]
        metadata: String,
    },

    /// Compare two experiments side-by-side
    Compare {
        /// First experiment name or ID
        left: String,

        /// Second experiment name or ID
        right: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Allow comparison across different dataset versions
        #[arg(long)]
        force: bool,
    },

    /// List experiments
    Experiments {
        /// Filter by dataset name
        #[arg(long)]
        dataset: Option<String>,
    },

    /// Show detailed experiment results
    Show {
        /// Experiment name or ID
        experiment: String,
    },

    /// Score a session's timeline outputs using evaluators (LLM-as-judge)
    Score {
        /// Session ID, prefix, or "latest"
        session: String,

        /// Evaluator names (can specify multiple: -e correctness -e safety)
        #[arg(short, long)]
        evaluator: Vec<String>,

        /// Score a specific timeline (ID, prefix, or label). Default: main
        #[arg(short, long)]
        timeline: Option<String>,

        /// Compare scores across ALL timelines in the session
        #[arg(long)]
        compare_timelines: bool,

        /// Expected output JSON for reference-based criteria (e.g., correctness)
        #[arg(long)]
        expected: Option<String>,

        /// Output results as JSON
        #[arg(long)]
        json: bool,

        /// Force re-scoring even if cached scores exist
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum DatasetAction {
    /// Create a new empty dataset
    Create {
        /// Dataset name
        name: String,

        /// Description
        #[arg(short, long, default_value = "")]
        description: String,
    },

    /// Add an example from a recorded session step
    AddFromSession {
        /// Dataset name
        dataset: String,

        /// Session ID, prefix, or "latest"
        #[arg(default_value = "latest")]
        session: String,

        /// Step number to extract input from
        #[arg(long)]
        input_step: u32,

        /// Step number to extract expected output from
        #[arg(long)]
        expected_step: Option<u32>,
    },

    /// Import examples from a JSONL file
    Import {
        /// Dataset name (created if doesn't exist)
        dataset: String,

        /// Path to JSONL file (each line: {"input": ..., "expected": ...})
        file: String,
    },

    /// Export a dataset to JSONL
    Export {
        /// Dataset name (or name@version)
        dataset: String,

        /// Output file path ("-" for stdout)
        #[arg(short, long, default_value = "-")]
        output: String,
    },

    /// List all datasets
    List,

    /// Show dataset details with example previews
    Show {
        /// Dataset name (or name@version)
        dataset: String,
    },

    /// Delete a dataset (all versions)
    Delete {
        /// Dataset name
        name: String,
    },
}

#[derive(Subcommand)]
enum EvaluatorAction {
    /// Create an evaluator
    Create {
        /// Evaluator name
        name: String,

        /// Type: exact_match, contains, regex, json_schema, tool_use_match, custom
        #[arg(short = 't', long = "type")]
        evaluator_type: String,

        /// Configuration JSON (depends on type). For custom: '{"command": "python judge.py"}'
        #[arg(short, long)]
        config: Option<String>,

        /// Description
        #[arg(short, long, default_value = "")]
        description: String,
    },

    /// List evaluators
    List,

    /// Delete an evaluator
    Delete {
        /// Evaluator name
        name: String,
    },
}

#[derive(Subcommand)]
enum HooksAction {
    /// Install Rewind hooks into Claude Code settings
    Install {
        /// Rewind server port
        #[arg(short, long, default_value = "4800")]
        port: u16,
    },

    /// Uninstall Rewind hooks from Claude Code settings
    Uninstall,

    /// Show status of Rewind hooks and server
    Status {
        /// Rewind server port to check
        #[arg(short, long, default_value = "4800")]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rewind=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Record { name, port, upstream, replay, web, web_port, insecure } => cmd_record(name, port, upstream, replay, web, web_port, insecure).await,
        Commands::Sessions => cmd_sessions(),
        Commands::Inspect { session } => cmd_inspect(session),
        Commands::Show { session, flat } => cmd_show(session, flat),
        Commands::Threads => cmd_threads(),
        Commands::Thread { thread_id } => cmd_thread(thread_id),
        Commands::Replay { session, from, upstream, port, label } => cmd_replay(session, from, upstream, port, label).await,
        Commands::Fork { session, at, label } => cmd_fork(session, at, label),
        Commands::Diff { session, left, right } => cmd_diff(session, left, right),
        Commands::Snapshot { directory, label } => cmd_snapshot(directory, label),
        Commands::Restore { snapshot } => cmd_restore(snapshot),
        Commands::Share { session, include_content, output, yes } => cmd_share(session, include_content, output, yes),
        Commands::Snapshots => cmd_snapshots(),
        Commands::Cache => cmd_cache(),
        Commands::Demo => cmd_demo(),
        Commands::Assert { action } => match action {
            AssertAction::Baseline { session, name, description } => cmd_assert_baseline(session, name, description),
            AssertAction::Check { session, against, token_tolerance, warn_model_change } => cmd_assert_check(session, against, token_tolerance, warn_model_change),
            AssertAction::List => cmd_assert_list(),
            AssertAction::Show { name } => cmd_assert_show(name),
            AssertAction::Delete { name } => cmd_assert_delete(name),
        },
        Commands::Eval { action } => match action {
            EvalAction::Dataset { action } => match action {
                DatasetAction::Create { name, description } => cmd_eval_dataset_create(name, description),
                DatasetAction::AddFromSession { dataset, session, input_step, expected_step } => cmd_eval_dataset_add_from_session(dataset, session, input_step, expected_step),
                DatasetAction::Import { dataset, file } => cmd_eval_dataset_import(dataset, file),
                DatasetAction::Export { dataset, output } => cmd_eval_dataset_export(dataset, output),
                DatasetAction::List => cmd_eval_dataset_list(),
                DatasetAction::Show { dataset } => cmd_eval_dataset_show(dataset),
                DatasetAction::Delete { name } => cmd_eval_dataset_delete(name),
            },
            EvalAction::Evaluator { action } => match action {
                EvaluatorAction::Create { name, evaluator_type, config, description } => cmd_eval_evaluator_create(name, evaluator_type, config, description),
                EvaluatorAction::List => cmd_eval_evaluator_list(),
                EvaluatorAction::Delete { name } => cmd_eval_evaluator_delete(name),
            },
            EvalAction::Run { dataset, command, evaluator, name, fail_below, timeout, json, metadata } => cmd_eval_run(dataset, command, evaluator, name, fail_below, timeout, json, metadata),
            EvalAction::Compare { left, right, json, force } => cmd_eval_compare(left, right, json, force),
            EvalAction::Experiments { dataset } => cmd_eval_experiments(dataset),
            EvalAction::Show { experiment } => cmd_eval_show(experiment),
            EvalAction::Score { session, evaluator, timeline, compare_timelines, expected, json, force } => cmd_eval_score(session, evaluator, timeline, compare_timelines, expected, json, force),
        },
        Commands::Web { port } => cmd_web(port).await,
        Commands::Query { sql, tables } => cmd_query(sql, tables),
        Commands::Hooks { action } => match action {
            HooksAction::Install { port } => cmd_hooks_install(port).await,
            HooksAction::Uninstall => cmd_hooks_uninstall(),
            HooksAction::Status { port } => cmd_hooks_status(port).await,
        },
        Commands::Export { action } => match action {
            ExportAction::Otel(args) => cmd_export_otel(args).await,
        },
        Commands::Import { action } => match action {
            ImportAction::Otel(args) => cmd_import_otel(args),
            ImportAction::FromLangfuse(args) => cmd_import_from_langfuse(args),
        },
        Commands::Fix { session, diagnosis_model, apply, command, upstream, port, step, expected, hypothesis, yes, json } => {
            cmd_fix(session, diagnosis_model, apply, command, upstream, port, step, expected, hypothesis, yes, json).await
        },
    }
}

async fn cmd_record(name: String, port: u16, upstream: String, replay: bool, web: bool, web_port: u16, insecure: bool) -> Result<()> {
    if insecure {
        eprintln!("  {} TLS certificate verification is disabled (--insecure)", "⚠".yellow().bold());
    }
    let store = Store::open_default().map_err(|e| {
        anyhow::anyhow!(
            "Failed to open Rewind store (~/.rewind/): {}. \
             Check disk space and file permissions.",
            e,
        )
    })?;
    let proxy = ProxyServer::new(store, &name, &upstream, replay, insecure)?;

    println!("{}", "⏪ Rewind — Recording Started".cyan().bold());
    println!();
    println!("  {} {}", "Session:".dimmed(), name.white().bold());
    println!("  {} {}", "Proxy:".dimmed(), format!("http://127.0.0.1:{}", port).yellow());
    println!("  {} {}", "Upstream:".dimmed(), upstream.dimmed());
    if replay {
        println!("  {} {}", "Replay:".dimmed(), "ON — identical requests served from cache (0 tokens)".green().bold());
    }
    if web {
        println!("  {} {}", "Dashboard:".dimmed(), format!("http://127.0.0.1:{}", web_port).cyan().bold());
    }
    println!();
    println!("  {} Set your agent's base URL to intercept calls:", "→".cyan());
    println!("    {}", format!("export OPENAI_BASE_URL=http://127.0.0.1:{}/v1", port).green());
    println!();
    println!("  {} to stop recording.", "Ctrl+C".yellow().bold());
    println!();

    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;

    if web {
        let web_store = Store::open_default()?;
        let (_event_tx, _) = tokio::sync::broadcast::channel::<rewind_web::StoreEvent>(256);
        let web_server = WebServer::new_standalone(web_store);
        let web_addr: SocketAddr = format!("127.0.0.1:{}", web_port).parse()?;

        tokio::select! {
            res = proxy.run(addr) => res,
            res = web_server.run(web_addr) => res,
        }
    } else {
        proxy.run(addr).await
    }
}

async fn cmd_web(port: u16) -> Result<()> {
    let store = Store::open_default()?;
    let web_server = WebServer::new_standalone(store);
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
    web_server.run(addr).await
}

async fn cmd_replay(session_ref: String, from_step: u32, upstream: String, port: u16, label: String) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    // Load all parent steps
    let engine = ReplayEngine::new(&store);
    let parent_steps = engine.get_full_timeline_steps(&timeline.id, &session.id)?;

    if from_step == 0 || from_step as usize > parent_steps.len() {
        bail!(
            "Invalid --from step {}. Session has {} steps (use 1-{}).",
            from_step, parent_steps.len(), parent_steps.len()
        );
    }

    // Create a forked timeline
    let fork = engine.fork(&session.id, &timeline.id, from_step, &label)?;

    // Start proxy in fork-and-execute mode
    let proxy = ProxyServer::new_fork_execute(
        store,
        &session.id,
        &fork.id,
        parent_steps.clone(),
        from_step,
        &upstream,
    )?;

    println!("{}", "⏪ Rewind — Fork & Execute Replay".cyan().bold());
    println!();
    println!("  {} {}", "Session:".dimmed(), session.name.white().bold());
    println!("  {} {}", "Fork at:".dimmed(), format!("Step {}", from_step).yellow());
    println!("  {} {}", "Cached:".dimmed(), format!("Steps 1-{} (0ms, 0 tokens)", from_step).green().bold());
    println!("  {} {}", "Live:".dimmed(), format!("Steps {}+ (forwarded to upstream)", from_step + 1).cyan());
    println!("  {} {}", "Fork ID:".dimmed(), fork.id[..12].to_string().dimmed());
    println!("  {} {}", "Proxy:".dimmed(), format!("http://127.0.0.1:{}", port).yellow());
    println!("  {} {}", "Upstream:".dimmed(), upstream.dimmed());
    println!();
    println!("  {} Point your agent at this proxy:", "→".cyan());
    println!("    {}", format!("export OPENAI_BASE_URL=http://127.0.0.1:{}/v1", port).green());
    println!();
    println!("  {} to stop. Then diff with:", "Ctrl+C".yellow().bold());
    println!("    {}", format!("rewind diff {} {} {}", &session.id[..8], &timeline.id[..8], &fork.id[..8]).green());
    println!();

    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
    proxy.run(addr).await
}

fn cmd_sessions() -> Result<()> {
    let store = Store::open_default()?;
    let sessions = store.list_sessions()?;

    if sessions.is_empty() {
        println!("{}", "No sessions recorded yet.".dimmed());
        println!("  Run {} to start recording.", "rewind record".green());
        return Ok(());
    }

    println!("{}", "⏪ Rewind Sessions".cyan().bold());
    println!();

    // Table header
    println!(
        "  {} {:^12} {:^20} {:>6} {:>10} {}",
        "STATUS".dimmed(),
        "ID".dimmed(),
        "NAME".dimmed(),
        "STEPS".dimmed(),
        "TOKENS".dimmed(),
        "CREATED".dimmed(),
    );
    println!("  {}", "─".repeat(72).dimmed());

    for session in &sessions {
        let status_icon = match session.status {
            rewind_store::SessionStatus::Recording => "●".yellow(),
            rewind_store::SessionStatus::Completed => "✓".green(),
            rewind_store::SessionStatus::Failed => "✗".red(),
            rewind_store::SessionStatus::Forked => "⑂".cyan(),
        };

        let short_id = &session.id[..12.min(session.id.len())];
        let ago = format_time_ago(session.created_at);

        println!(
            "  {}  {:>12} {:>20} {:>6} {:>10} {}",
            status_icon,
            short_id.dimmed(),
            session.name.white().bold(),
            session.total_steps.to_string().yellow(),
            session.total_tokens.to_string().blue(),
            ago.dimmed(),
        );
    }

    println!();
    println!("  Run {} to inspect a session.", "rewind inspect <session-id>".green());
    println!("  Web: {}", "\x1b]8;;http://127.0.0.1:4800\x1b\\http://127.0.0.1:4800\x1b]8;;\x1b\\".cyan());
    Ok(())
}

fn cmd_inspect(session_ref: String) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let mut app = TuiApp::new(store, &session.id, &timeline.id)?;
    app.run()
}

fn cmd_show(session_ref: String, flat: bool) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let engine = ReplayEngine::new(&store);
    let steps = engine.get_full_timeline_steps(&timeline.id, &session.id)?;
    let spans = engine.get_full_timeline_spans(&timeline.id, &session.id)?;

    println!("{}", "⏪ Rewind — Session Trace".cyan().bold());
    println!();
    println!("  {} {}", "Session:".dimmed(), session.name.white().bold());
    println!("  {} {}", "ID:".dimmed(), session.id.dimmed());
    println!("  {} {}", "Steps:".dimmed(), steps.len().to_string().yellow());
    println!("  {} {}", "Tokens:".dimmed(), session.total_tokens.to_string().blue());

    if !spans.is_empty() && !flat {
        let agent_names: Vec<&str> = spans.iter()
            .filter(|s| s.span_type == rewind_store::SpanType::Agent)
            .map(|s| s.name.as_str())
            .collect();
        if !agent_names.is_empty() {
            println!("  {} {}", "Agents:".dimmed(), agent_names.join(", ").cyan());
        }
        println!();
        render_span_tree(&spans, &steps, &store, 1);
    } else {
        println!();
        for step in &steps {
            let status_icon = match step.status {
                rewind_store::StepStatus::Success => "✓".green(),
                rewind_store::StepStatus::Error => "✗".red(),
                rewind_store::StepStatus::Pending => "…".yellow(),
            };

            let connector = if step.step_number == 1 { "┌" } else if step.step_number == steps.len() as u32 { "└" } else { "├" };

            println!(
                "  {} {} {} {:>8} {:>8} {:>10} {}",
                connector.dimmed(),
                status_icon,
                step.step_type.icon(),
                format!("Step {}", step.step_number).white().bold(),
                step.model.magenta(),
                format!("{}ms", step.duration_ms).yellow(),
                format!("{}↓ {}↑", step.tokens_in, step.tokens_out).blue(),
            );

            if let Some(ref err) = step.error {
                println!("  │   {} {}", "ERROR:".red().bold(), err.red());
            }

            if let Ok(data) = store.blobs.get(&step.response_blob)
                && let Ok(json_str) = String::from_utf8(data)
                    && let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        let preview = extract_response_preview(&val);
                        if !preview.is_empty() {
                            let truncated: String = preview.chars().take(100).collect();
                            println!("  │   {} {}", "→".dimmed(), truncated.dimmed());
                        }
                    }
        }
    }

    // ── Replay savings (if session has forks) ─────────────────
    let timelines = store.get_timelines(&session.id)?;
    let forks: Vec<&Timeline> = timelines.iter()
        .filter(|t| t.parent_timeline_id.is_some() && t.fork_at_step.is_some())
        .collect();
    if !forks.is_empty() {
        print_session_savings(&store, &forks);
    }

    println!();
    println!("  Run {} to explore interactively.", format!("rewind inspect {}", &session.id[..8]).green());
    let web_url = format!("http://127.0.0.1:4800/#/session/{}", session.id);
    println!("  Web: {}", format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", web_url, web_url).cyan());
    Ok(())
}

fn print_session_savings(store: &Store, forks: &[&Timeline]) {
    use rewind_proxy::pricing::{compute_savings, ReplaySavings};

    let mut cumulative = ReplaySavings {
        steps_total: 0,
        steps_cached: 0,
        steps_live: 0,
        tokens_saved: 0,
        cost_saved_usd: 0.0,
        time_saved_ms: 0,
    };

    for fork in forks {
        let parent_id = match &fork.parent_timeline_id {
            Some(id) => id,
            None => continue,
        };
        let fork_at = match fork.fork_at_step {
            Some(n) => n,
            None => continue,
        };
        let parent_steps = match store.get_steps(parent_id) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let own_steps = match store.get_steps(&fork.id) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let cached: Vec<_> = parent_steps.into_iter()
            .filter(|s| s.step_number <= fork_at)
            .collect();
        let savings = compute_savings(&cached, &own_steps);

        cumulative.steps_total += savings.steps_total;
        cumulative.steps_cached += savings.steps_cached;
        cumulative.steps_live += savings.steps_live;
        cumulative.tokens_saved += savings.tokens_saved;
        cumulative.cost_saved_usd += savings.cost_saved_usd;
        cumulative.time_saved_ms += savings.time_saved_ms;
    }

    if cumulative.steps_cached == 0 {
        return;
    }

    println!();
    println!("  {}", "⏪ Replay Savings".cyan().bold());
    println!(
        "    {} {} cached (served from fork cache)",
        "Steps:".dimmed(),
        format!("{}/{}", cumulative.steps_cached, cumulative.steps_total).yellow(),
    );
    println!(
        "    {} {}",
        "Tokens saved:".dimmed(),
        format!("{}", cumulative.tokens_saved).blue(),
    );
    println!(
        "    {} {}",
        "Cost saved:".dimmed(),
        format!("${:.2}", cumulative.cost_saved_usd).green().bold(),
    );

    let secs = cumulative.time_saved_ms / 1000;
    let time_str = if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}.{}s", secs, (cumulative.time_saved_ms % 1000) / 100)
    };
    println!(
        "    {} {}",
        "Time saved:".dimmed(),
        time_str.yellow(),
    );

    if forks.len() > 1 {
        println!(
            "    {} across {} replays",
            "Cumulative:".dimmed(),
            forks.len().to_string().white().bold(),
        );
    }
}

fn render_span_tree(spans: &[rewind_store::Span], steps: &[rewind_store::Step], store: &Store, indent: usize) {
    let root_spans: Vec<&rewind_store::Span> = spans.iter()
        .filter(|s| s.parent_span_id.is_none())
        .collect();

    if root_spans.is_empty() {
        for step in steps {
            render_step_in_tree(step, store, indent, false);
        }
        return;
    }

    for span in &root_spans {
        render_span_node(span, spans, steps, store, indent);
    }
}

fn render_span_node(span: &rewind_store::Span, all_spans: &[rewind_store::Span], all_steps: &[rewind_store::Step], store: &Store, indent: usize) {
    let prefix = "  ".repeat(indent);
    let duration = if span.duration_ms > 0 {
        format!("{}ms", span.duration_ms)
    } else {
        "…".to_string()
    };

    let status_icon = match span.status.as_str() {
        "completed" => "✓".green(),
        "error" => "✗".red(),
        _ => "…".yellow(),
    };

    println!(
        "{}▼ {} {} {} ({})  {}  {}",
        prefix,
        status_icon,
        span.span_type.icon(),
        span.name.white().bold(),
        span.span_type.as_str().dimmed(),
        duration.yellow(),
        if let Some(ref err) = span.error { err.red().to_string() } else { String::new() },
    );

    let child_spans: Vec<&rewind_store::Span> = all_spans.iter()
        .filter(|s| s.parent_span_id.as_deref() == Some(&span.id))
        .collect();

    let span_steps: Vec<&rewind_store::Step> = all_steps.iter()
        .filter(|s| s.span_id.as_deref() == Some(&span.id))
        .collect();

    let mut child_idx = 0;
    let mut step_idx = 0;

    while child_idx < child_spans.len() || step_idx < span_steps.len() {
        let show_step = if child_idx >= child_spans.len() {
            true
        } else if step_idx >= span_steps.len() {
            false
        } else {
            span_steps[step_idx].created_at <= child_spans[child_idx].started_at
        };

        if show_step {
            let is_last = step_idx == span_steps.len() - 1 && child_idx >= child_spans.len();
            render_step_in_tree(span_steps[step_idx], store, indent + 1, is_last);
            step_idx += 1;
        } else {
            render_span_node(child_spans[child_idx], all_spans, all_steps, store, indent + 1);
            child_idx += 1;
        }
    }
}

fn render_step_in_tree(step: &rewind_store::Step, _store: &Store, indent: usize, is_last: bool) {
    let prefix = "  ".repeat(indent);
    let connector = if is_last { "└" } else { "├" };

    let status_icon = match step.status {
        rewind_store::StepStatus::Success => "✓".green(),
        rewind_store::StepStatus::Error => "✗".red(),
        rewind_store::StepStatus::Pending => "…".yellow(),
    };

    let token_info = if step.tokens_in > 0 || step.tokens_out > 0 {
        format!("  {}↓ {}↑", step.tokens_in, step.tokens_out).blue().to_string()
    } else {
        String::new()
    };

    println!(
        "{}{} {} {}  {}  {}{}",
        prefix,
        connector.dimmed(),
        status_icon,
        step.step_type.icon(),
        step.model.magenta(),
        format!("{}ms", step.duration_ms).yellow(),
        token_info,
    );

    if let Some(ref err) = step.error {
        println!("{}│   {} {}", prefix, "ERROR:".red().bold(), err.red());
    }
}

fn cmd_threads() -> Result<()> {
    let store = Store::open_default()?;
    let thread_ids = store.list_thread_ids()?;

    if thread_ids.is_empty() {
        println!("{}", "No conversation threads found.".dimmed());
        return Ok(());
    }

    println!("{}", "⏪ Rewind — Conversation Threads".cyan().bold());
    println!();

    for tid in &thread_ids {
        let sessions = store.get_sessions_by_thread(tid)?;
        let total_steps: u32 = sessions.iter().map(|s| s.total_steps).sum();
        let total_tokens: u64 = sessions.iter().map(|s| s.total_tokens).sum();

        println!(
            "  {} {} ({} sessions, {} steps, {} tokens)",
            "🧵".dimmed(),
            tid.white().bold(),
            sessions.len().to_string().yellow(),
            total_steps.to_string().yellow(),
            total_tokens.to_string().blue(),
        );
    }
    println!();
    Ok(())
}

fn cmd_thread(thread_id: String) -> Result<()> {
    let store = Store::open_default()?;
    let sessions = store.get_sessions_by_thread(&thread_id)?;

    if sessions.is_empty() {
        println!("Thread not found: {}", thread_id);
        return Ok(());
    }

    println!("{}", "⏪ Rewind — Thread Detail".cyan().bold());
    println!();
    println!("  {} {}", "Thread:".dimmed(), thread_id.white().bold());
    println!("  {} {}", "Sessions:".dimmed(), sessions.len().to_string().yellow());
    println!();

    for (i, session) in sessions.iter().enumerate() {
        let status_icon = match session.status {
            rewind_store::SessionStatus::Recording => "●".yellow(),
            rewind_store::SessionStatus::Completed => "✓".green(),
            rewind_store::SessionStatus::Failed => "✗".red(),
            rewind_store::SessionStatus::Forked => "⑂".cyan(),
        };

        println!(
            "  {} Turn {} — {} {} ({} steps, {} tokens)",
            status_icon,
            i + 1,
            session.name.white().bold(),
            format!("[{}]", &session.id[..8]).dimmed(),
            session.total_steps.to_string().yellow(),
            session.total_tokens.to_string().blue(),
        );
    }
    println!();
    Ok(())
}

fn cmd_fork(session_ref: String, at_step: u32, label: String) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let engine = ReplayEngine::new(&store);
    let fork = engine.fork(&session.id, &timeline.id, at_step, &label)?;

    println!("{}", "⏪ Rewind — Fork Created".cyan().bold());
    println!();
    println!("  {} {}", "Fork ID:".dimmed(), fork.id.white().bold());
    println!("  {} {}", "Label:".dimmed(), label.cyan());
    println!("  {} {}", "Forked at:".dimmed(), format!("Step {}", at_step).yellow());
    println!("  {} Steps 1-{} are shared with the parent timeline.", "→".cyan(), at_step);
    println!();
    println!(
        "  To diff: {}",
        format!("rewind diff {} {} {}", &session.id[..8], &timeline.id[..8], &fork.id[..8]).green()
    );
    Ok(())
}

fn cmd_diff(session_ref: String, left_ref: String, right_ref: String) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timelines = store.get_timelines(&session.id)?;

    let left_id = resolve_timeline_ref(&timelines, &left_ref)?;
    let right_id = resolve_timeline_ref(&timelines, &right_ref)?;

    let engine = ReplayEngine::new(&store);
    let diff = engine.diff_timelines(&session.id, &left_id, &right_id)?;

    println!("{}", "⏪ Rewind — Timeline Diff".cyan().bold());
    println!();
    println!(
        "  {} {} {} {}",
        diff.left_label.cyan().bold(),
        "vs".dimmed(),
        diff.right_label.yellow().bold(),
        if let Some(step) = diff.diverge_at_step {
            format!("(diverge at step {})", step).red().to_string()
        } else {
            "(identical)".green().to_string()
        },
    );
    println!();

    for sd in &diff.step_diffs {
        let icon: colored::ColoredString = match sd.diff_type {
            rewind_replay::DiffType::Same => "═".dimmed(),
            rewind_replay::DiffType::Modified => "≠".yellow().bold(),
            rewind_replay::DiffType::LeftOnly => "←".cyan(),
            rewind_replay::DiffType::RightOnly => "→".magenta(),
        };

        let step_str = format!("Step {:>2}", sd.step_number);
        println!(
            "  {} {} {}",
            icon,
            step_str.white().bold(),
            match sd.diff_type {
                rewind_replay::DiffType::Same => "identical".dimmed().to_string(),
                rewind_replay::DiffType::Modified => {
                    let left_status = sd.left.as_ref().map(|s| s.status.clone()).unwrap_or_default();
                    let right_status = sd.right.as_ref().map(|s| s.status.clone()).unwrap_or_default();
                    let left_tokens = sd.left.as_ref().map(|s| format!("{}tok", s.tokens_in + s.tokens_out)).unwrap_or_default();
                    let right_tokens = sd.right.as_ref().map(|s| format!("{}tok", s.tokens_in + s.tokens_out)).unwrap_or_default();
                    format!("{} {} {} {} {}",
                        format!("[{}]", left_status).red(),
                        left_tokens.dimmed(),
                        "→".dimmed(),
                        format!("[{}]", right_status).green(),
                        right_tokens.dimmed(),
                    )
                }
                rewind_replay::DiffType::LeftOnly => format!("only in {}", diff.left_label).cyan().to_string(),
                rewind_replay::DiffType::RightOnly => format!("only in {}", diff.right_label).magenta().to_string(),
            }
        );
    }

    println!();
    Ok(())
}

fn cmd_demo() -> Result<()> {
    println!("{}", "⏪ Rewind — Seeding Demo Data".cyan().bold());
    println!();

    let store = Store::open_default()?;
    seed_demo_data(&store)?;

    // Create a demo baseline from the session
    let session = store.get_latest_session()?.context("No session after demo seed")?;
    let timeline = store.get_root_timeline(&session.id)?.context("No timeline")?;
    let manager = BaselineManager::new(&store);
    // Only create if it doesn't already exist
    if manager.get_baseline("demo-baseline")?.is_none() {
        manager.create_baseline(&session.id, &timeline.id, "demo-baseline", "Demo baseline for regression testing")?;
        println!("  {} Created demo baseline: {}", "✓".green(), "demo-baseline".cyan());
    }

    println!("  {} Demo session created!", "✓".green().bold());
    println!();
    println!("  Try these commands:");
    println!("    {} — list all sessions", "rewind sessions".green());
    println!("    {} — see the trace", "rewind show latest".green());
    println!("    {} — interactive TUI", "rewind inspect latest".green());
    println!("    {} — web dashboard", "rewind web".green());
    println!("    {} — check for regressions", "rewind assert check latest --against demo-baseline".green());
    println!();
    Ok(())
}

// ── Snapshot & Restore ────────────────────────────────────────

fn cmd_snapshot(directory: String, label: String) -> Result<()> {
    let dir = std::path::Path::new(&directory).canonicalize()
        .context(format!("Directory not found: {}", directory))?;

    println!("{}", "⏪ Rewind — Creating Snapshot".cyan().bold());
    println!();
    println!("  {} {}", "Directory:".dimmed(), dir.display().to_string().white().bold());
    println!("  {} {}", "Label:".dimmed(), label.cyan());

    // Create a tar.gz of the directory in memory
    let mut archive_data = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut archive_data, flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);

        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                // Skip hidden dirs, target, node_modules, .git, __pycache__
                !(name.starts_with('.') || name == "target" || name == "node_modules" || name == "__pycache__")
            })
        {
            let entry = entry?;
            if entry.file_type().is_file() {
                let relative = entry.path().strip_prefix(&dir)?;
                tar.append_path_with_name(entry.path(), relative)?;
            }
        }
        tar.finish()?;
    }

    let size_bytes = archive_data.len() as u64;
    let store = Store::open_default()?;
    let blob_hash = store.blobs.put(&archive_data)?;

    // Count files (walk again is wasteful but simple)
    let file_count = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(name.starts_with('.') || name == "target" || name == "node_modules" || name == "__pycache__")
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count() as u32;

    let snapshot = rewind_store::Snapshot::new(&label, &dir.to_string_lossy(), &blob_hash, file_count, size_bytes);
    let snap_id = snapshot.id.clone();
    store.create_snapshot(&snapshot)?;

    println!("  {} {}", "Files:".dimmed(), file_count.to_string().yellow());
    println!("  {} {}", "Size:".dimmed(), format_bytes(size_bytes).yellow());
    println!("  {} {}", "ID:".dimmed(), snap_id[..12].to_string().dimmed());
    println!();
    println!("  {} Snapshot saved!", "✓".green().bold());
    println!("  Restore with: {}", format!("rewind restore {}", &snap_id[..8]).green());
    println!();
    Ok(())
}

fn cmd_restore(snapshot_ref: String) -> Result<()> {
    let store = Store::open_default()?;
    let snapshot = store.get_snapshot(&snapshot_ref)?
        .context(format!("Snapshot not found: {}", snapshot_ref))?;

    println!("{}", "⏪ Rewind — Restoring Snapshot".cyan().bold());
    println!();
    println!("  {} {}", "Label:".dimmed(), snapshot.label.cyan());
    println!("  {} {}", "Directory:".dimmed(), snapshot.directory.white().bold());
    println!("  {} {}", "Files:".dimmed(), snapshot.file_count.to_string().yellow());

    let archive_data = store.blobs.get(&snapshot.blob_hash)?;

    let decoder = flate2::read::GzDecoder::new(&archive_data[..]);
    let mut archive = tar::Archive::new(decoder);

    let target_dir = std::path::Path::new(&snapshot.directory)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&snapshot.directory));

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?;
        if entry_path.components().any(|c| c == std::path::Component::ParentDir)
            || entry_path.is_absolute()
        {
            bail!(
                "Refusing to extract archive entry with path traversal: {}",
                entry_path.display()
            );
        }
        entry.unpack_in(&target_dir)?;
    }

    println!();
    println!("  {} Restored to {}", "✓".green().bold(), snapshot.directory.white());
    println!();
    Ok(())
}

fn cmd_snapshots() -> Result<()> {
    let store = Store::open_default()?;
    let snapshots = store.list_snapshots()?;

    if snapshots.is_empty() {
        println!("{}", "No snapshots yet.".dimmed());
        println!("  Run {} to create one.", "rewind snapshot".green());
        return Ok(());
    }

    println!("{}", "⏪ Rewind Snapshots".cyan().bold());
    println!();
    println!(
        "  {:>12} {:>15} {:>6} {:>10} {}",
        "ID".dimmed(), "LABEL".dimmed(), "FILES".dimmed(), "SIZE".dimmed(), "CREATED".dimmed(),
    );
    println!("  {}", "─".repeat(65).dimmed());

    for snap in &snapshots {
        let short_id = &snap.id[..12.min(snap.id.len())];
        let ago = format_time_ago(snap.created_at);
        println!(
            "  {:>12} {:>15} {:>6} {:>10} {}",
            short_id.dimmed(),
            snap.label.cyan(),
            snap.file_count.to_string().yellow(),
            format_bytes(snap.size_bytes).yellow(),
            ago.dimmed(),
        );
    }
    println!();
    Ok(())
}

fn cmd_cache() -> Result<()> {
    let store = Store::open_default()?;
    let stats = store.cache_stats()?;

    println!("{}", "⏪ Rewind — Instant Replay Cache".cyan().bold());
    println!();
    if stats.entries == 0 {
        println!("  {} Cache is empty.", "○".dimmed());
        println!("  Run {} to enable.", "rewind record --replay".green());
    } else {
        println!("  {} {}", "Cached responses:".dimmed(), stats.entries.to_string().white().bold());
        println!("  {} {}", "Total cache hits:".dimmed(), stats.total_hits.to_string().yellow().bold());
        println!("  {} {}", "Tokens saved:".dimmed(), stats.total_tokens_saved.to_string().green().bold());
    }
    println!();
    Ok(())
}

fn cmd_query(sql: Option<String>, tables: bool) -> Result<()> {
    let store = Store::open_default()?;

    if tables {
        println!("{}", "⏪ Rewind — Database Tables".cyan().bold());
        println!();
        let table_names = store.list_tables()?;
        for name in &table_names {
            println!("  {}", name.white().bold());
            // Show column info via PRAGMA
            let result = store.query_raw(&format!("PRAGMA table_info({})", name))?;
            for row in &result.rows {
                // PRAGMA table_info returns: cid, name, type, notnull, dflt_value, pk
                let col_name = row.get(1).map(|s| s.as_str()).unwrap_or("?");
                let col_type = row.get(2).map(|s| s.as_str()).unwrap_or("?");
                let is_pk = row.get(5).map(|s| s == "1").unwrap_or(false);
                println!(
                    "    {} {} {}",
                    col_name.cyan(),
                    col_type.dimmed(),
                    if is_pk { "PK".yellow().to_string() } else { String::new() },
                );
            }
            println!();
        }
        return Ok(());
    }

    let sql = match sql {
        Some(s) => s,
        None => {
            println!("{}", "⏪ Rewind — Query".cyan().bold());
            println!();
            println!("  {} Provide a SQL query or use {} to see tables.", "Usage:".dimmed(), "--tables".green());
            println!();
            println!("  {}", "Examples:".dimmed());
            println!("    {}", r#"rewind query "SELECT * FROM sessions""#.green());
            println!("    {}", r#"rewind query "SELECT model, COUNT(*) as calls FROM steps GROUP BY model""#.green());
            println!("    {}", r#"rewind query "SELECT step_type, AVG(duration_ms) as avg_ms FROM steps GROUP BY step_type""#.green());
            println!("    {}", r#"rewind query --tables"#.green());
            println!();
            return Ok(());
        }
    };

    let result = store.query_raw(&sql)?;

    if result.rows.is_empty() {
        println!("{}", "(no results)".dimmed());
        return Ok(());
    }

    // Calculate column widths
    let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();
    for row in &result.rows {
        for (i, val) in row.iter().enumerate() {
            // Truncate display at 60 chars for readability
            let display_len = val.chars().take(60).count();
            if display_len > widths[i] {
                widths[i] = display_len;
            }
        }
    }

    // Print header
    let header: String = result
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:width$}", c, width = widths[i]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  {}", header.dimmed());
    let separator: String = widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("──");
    println!("  {}", separator.dimmed());

    // Print rows
    for row in &result.rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, val)| {
                let truncated: String = val.chars().take(60).collect();
                let suffix = if val.chars().count() > 60 { "…" } else { "" };
                format!("{:width$}{}", truncated, suffix, width = widths[i])
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("  {}", line);
    }

    println!();
    println!("  {} row(s)", result.rows.len().to_string().yellow());
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ── Assert commands ──────────────────────────────────────────

fn cmd_assert_baseline(session_ref: String, name: String, description: String) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let manager = BaselineManager::new(&store);
    let baseline = manager.create_baseline(&session.id, &timeline.id, &name, &description)?;

    println!("{}", "⏪ Rewind — Baseline Created".cyan().bold());
    println!();
    println!("  {} {}", "Name:".dimmed(), baseline.name.white().bold());
    println!("  {} {}", "Source:".dimmed(), session.name.dimmed());
    println!("  {} {}", "Steps:".dimmed(), baseline.step_count.to_string().yellow());
    println!("  {} {}", "Tokens:".dimmed(), baseline.total_tokens.to_string().blue());
    println!();
    println!("  Check against it: {}", format!("rewind assert check latest --against {}", name).green());
    Ok(())
}

fn cmd_assert_check(session_ref: String, against: String, token_tolerance: u32, warn_model_change: bool) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let manager = BaselineManager::new(&store);
    let baseline = manager.get_baseline(&against)?
        .context(format!("Baseline '{}' not found", against))?;
    let baseline_steps = manager.get_baseline_steps(&baseline.id)?;

    let engine = ReplayEngine::new(&store);
    let actual_steps = engine.get_full_timeline_steps(&timeline.id, &session.id)?;

    let tolerance = Tolerance::default()
        .with_token_pct(token_tolerance);
    let tolerance = Tolerance {
        model_change_is_warning: warn_model_change,
        ..tolerance
    };

    let checker = AssertionEngine::new(&store, tolerance);
    let result = checker.check(
        &baseline.id,
        &baseline.name,
        &baseline_steps,
        &actual_steps,
        &session.id,
        &timeline.id,
    )?;

    // Print header
    println!("{}", "⏪ Rewind — Assertion Check".cyan().bold());
    println!();
    println!(
        "  {} {} ({})",
        "Baseline:".dimmed(),
        baseline.name.white().bold(),
        format!("{} steps", baseline.step_count).dimmed(),
    );
    let short_id = &session.id[..12.min(session.id.len())];
    println!(
        "  {} {} ({})",
        "Session:".dimmed(),
        short_id.white().bold(),
        session.name.dimmed(),
    );
    println!(
        "  {} tokens ±{}%, model changes = {}",
        "Tolerance:".dimmed(),
        token_tolerance,
        if warn_model_change { "warn" } else { "fail" },
    );
    println!();

    // Print per-step results
    use rewind_assert::checker::StepVerdict;
    for (i, step_result) in result.step_results.iter().enumerate() {
        let connector = if i == 0 {
            "┌"
        } else if i == result.step_results.len() - 1 {
            "└"
        } else {
            "├"
        };

        let verdict_str = match &step_result.verdict {
            StepVerdict::Pass => format!("{} PASS", "✓".green()),
            StepVerdict::Warn => format!("{} WARN", "⚠".yellow()),
            StepVerdict::Fail => format!("{} FAIL", "✗".red().bold()),
            StepVerdict::Missing => format!("{} MISSING", "∅".red()),
            StepVerdict::Extra => format!("{} EXTRA", "+".yellow()),
        };

        // Find step type from baseline or actual
        let step_type_label = if let Some(bs) = baseline_steps.iter().find(|s| s.step_number == step_result.step_number) {
            match bs.step_type.as_str() {
                "llm_call" => "🧠 LLM Call",
                "tool_call" => "🔧 Tool Call",
                "tool_result" => "📋 Tool Result",
                _ => "  Step",
            }
        } else {
            "  Step"
        };

        // Find the most important check message
        let detail = step_result.checks.iter()
            .find(|c| !c.passed)
            .or_else(|| step_result.checks.first())
            .map(|c| c.message.clone())
            .unwrap_or_default();

        println!(
            "  {} Step {:>2}  {}  {}  {}",
            connector.dimmed(),
            step_result.step_number,
            step_type_label,
            verdict_str,
            detail.dimmed(),
        );
    }

    // Summary
    println!();
    if result.passed {
        println!(
            "  {} {} ({} passed, {} warnings)",
            "Result:".dimmed(),
            "PASSED".green().bold(),
            result.summary.passed_checks,
            result.summary.warnings,
        );
    } else {
        println!(
            "  {} {} ({} passed, {} failed, {} warnings)",
            "Result:".dimmed(),
            "FAILED".red().bold(),
            result.summary.passed_checks,
            result.summary.failed_checks,
            result.summary.warnings,
        );
    }
    println!();

    if !result.passed {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_assert_list() -> Result<()> {
    let store = Store::open_default()?;
    let manager = BaselineManager::new(&store);
    let baselines = manager.list_baselines()?;

    if baselines.is_empty() {
        println!("{}", "No baselines yet.".dimmed());
        println!("  Create one: {}", "rewind assert baseline latest --name my-baseline".green());
        return Ok(());
    }

    println!("{}", "⏪ Rewind Baselines".cyan().bold());
    println!();
    println!(
        "  {:>20} {:>12} {:>6} {:>10} {}",
        "NAME".dimmed(), "SOURCE".dimmed(), "STEPS".dimmed(), "TOKENS".dimmed(), "CREATED".dimmed(),
    );
    println!("  {}", "─".repeat(65).dimmed());

    for bl in &baselines {
        let short_src = &bl.source_session_id[..12.min(bl.source_session_id.len())];
        let ago = format_time_ago(bl.created_at);
        println!(
            "  {:>20} {:>12} {:>6} {:>10} {}",
            bl.name.white().bold(),
            short_src.dimmed(),
            bl.step_count.to_string().yellow(),
            bl.total_tokens.to_string().blue(),
            ago.dimmed(),
        );
    }
    println!();
    Ok(())
}

fn cmd_assert_show(name: String) -> Result<()> {
    let store = Store::open_default()?;
    let manager = BaselineManager::new(&store);
    let baseline = manager.get_baseline(&name)?
        .context(format!("Baseline '{}' not found", name))?;
    let steps = manager.get_baseline_steps(&baseline.id)?;

    println!("{}", "⏪ Rewind — Baseline Detail".cyan().bold());
    println!();
    println!("  {} {}", "Name:".dimmed(), baseline.name.white().bold());
    println!("  {} {}", "ID:".dimmed(), baseline.id.dimmed());
    println!("  {} {}", "Source Session:".dimmed(), baseline.source_session_id.dimmed());
    println!("  {} {}", "Steps:".dimmed(), baseline.step_count.to_string().yellow());
    println!("  {} {}", "Tokens:".dimmed(), baseline.total_tokens.to_string().blue());
    if !baseline.description.is_empty() {
        println!("  {} {}", "Description:".dimmed(), baseline.description);
    }
    println!();
    println!("  {}", "Expected Steps:".dimmed());

    for step in &steps {
        let type_icon = match step.step_type.as_str() {
            "llm_call" => "🧠",
            "tool_call" => "🔧",
            "tool_result" => "📋",
            _ => "  ",
        };

        let tool_info = step.tool_name.as_deref()
            .map(|t| format!(" → {}", t.cyan()))
            .unwrap_or_default();

        println!(
            "    Step {:>2}  {} {:>10}  {}↓ {}↑{}{}",
            step.step_number,
            type_icon,
            step.expected_model.magenta(),
            step.tokens_in.to_string().blue(),
            step.tokens_out.to_string().blue(),
            tool_info,
            if step.has_error { format!(" {}", "ERROR".red()) } else { String::new() },
        );
    }
    println!();
    Ok(())
}

fn cmd_assert_delete(name: String) -> Result<()> {
    let store = Store::open_default()?;
    let manager = BaselineManager::new(&store);
    manager.delete_baseline(&name)?;

    println!("  {} Baseline '{}' deleted.", "✓".green(), name);
    Ok(())
}

// ── Eval commands ────────────────────────────────────────────

fn cmd_eval_dataset_create(name: String, description: String) -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    let dataset = mgr.create(&name, &description)?;

    println!("{}", "⏪ Rewind — Dataset Created".cyan().bold());
    println!();
    println!("  {} {}", "Name:".dimmed(), dataset.name.white().bold());
    println!("  {} {}", "Version:".dimmed(), dataset.version.to_string().yellow());
    if !description.is_empty() {
        println!("  {} {}", "Description:".dimmed(), description);
    }
    println!();
    println!("  Add examples: {}", format!("rewind eval dataset import {} examples.jsonl", name).green());
    Ok(())
}

fn cmd_eval_dataset_add_from_session(dataset: String, session: String, input_step: u32, expected_step: Option<u32>) -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    let example = mgr.import_from_session(&dataset, &session, input_step, expected_step)?;

    println!("{}", "⏪ Rewind — Example Added from Session".cyan().bold());
    println!();
    println!("  {} {}", "Dataset:".dimmed(), dataset.white().bold());
    println!("  {} {}", "Ordinal:".dimmed(), example.ordinal.to_string().yellow());
    println!("  {} step {}", "Input from:".dimmed(), input_step.to_string().cyan());
    println!("  {} step {}", "Expected from:".dimmed(), expected_step.unwrap_or(input_step).to_string().cyan());
    println!();
    Ok(())
}

fn cmd_eval_dataset_import(dataset: String, file: String) -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    let path = std::path::Path::new(&file);
    let ds = mgr.import_from_jsonl(&dataset, path)?;

    println!("{}", "⏪ Rewind — Dataset Imported".cyan().bold());
    println!();
    println!("  {} {}", "Dataset:".dimmed(), ds.name.white().bold());
    println!("  {} {}", "Version:".dimmed(), ds.version.to_string().yellow());
    println!("  {} {}", "Examples:".dimmed(), ds.example_count.to_string().green().bold());
    println!("  {} {}", "Source:".dimmed(), file.dimmed());
    println!();
    Ok(())
}

fn cmd_eval_dataset_export(dataset_ref: String, output: String) -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    let (name, version) = rewind_eval::dataset::parse_dataset_ref(&dataset_ref);

    if output == "-" {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        mgr.export_jsonl(name, version, &mut handle)?;
    } else {
        let mut file = std::fs::File::create(&output)?;
        mgr.export_jsonl(name, version, &mut file)?;
        println!("  {} Exported to {}", "✓".green(), output);
    }
    Ok(())
}

fn cmd_eval_dataset_list() -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    let datasets = mgr.list()?;

    if datasets.is_empty() {
        println!("{}", "No datasets yet.".dimmed());
        println!("  Create one: {}", "rewind eval dataset create my-dataset".green());
        return Ok(());
    }

    println!("{}", "⏪ Rewind — Evaluation Datasets".cyan().bold());
    println!();
    println!(
        "  {:>20} {:>8} {:>10} {}",
        "NAME".dimmed(), "VERSION".dimmed(), "EXAMPLES".dimmed(), "UPDATED".dimmed(),
    );
    println!("  {}", "─".repeat(55).dimmed());

    for ds in &datasets {
        let ago = format_time_ago(ds.updated_at);
        println!(
            "  {:>20} {:>8} {:>10} {}",
            ds.name.white().bold(),
            format!("v{}", ds.version).yellow(),
            ds.example_count.to_string().green(),
            ago.dimmed(),
        );
    }
    println!();
    Ok(())
}

fn cmd_eval_dataset_show(dataset_ref: String) -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    let (name, version) = rewind_eval::dataset::parse_dataset_ref(&dataset_ref);
    let dataset = mgr.get(name, version)?
        .context(format!("Dataset '{}' not found", dataset_ref))?;
    let examples = mgr.get_examples(&dataset.id)?;

    println!("{}", "⏪ Rewind — Dataset Detail".cyan().bold());
    println!();
    println!("  {} {}", "Name:".dimmed(), dataset.name.white().bold());
    println!("  {} {}", "Version:".dimmed(), format!("v{}", dataset.version).yellow());
    println!("  {} {}", "Examples:".dimmed(), dataset.example_count.to_string().green());
    if !dataset.description.is_empty() {
        println!("  {} {}", "Description:".dimmed(), dataset.description);
    }
    println!();

    if examples.is_empty() {
        println!("  {}", "(no examples)".dimmed());
    } else {
        println!("  {}", "Examples:".dimmed());
        for ex in &examples {
            let (input, expected) = mgr.resolve_example(ex)?;
            let input_str = serde_json::to_string(&input).unwrap_or_default();
            let expected_str = serde_json::to_string(&expected).unwrap_or_default();
            let input_preview: String = input_str.chars().take(60).collect();
            let expected_preview: String = expected_str.chars().take(60).collect();
            println!(
                "    {} {} → {}",
                format!("#{}", ex.ordinal).yellow(),
                input_preview.cyan(),
                expected_preview.dimmed(),
            );
        }
    }
    println!();
    Ok(())
}

fn cmd_eval_dataset_delete(name: String) -> Result<()> {
    let store = Store::open_default()?;
    let mgr = DatasetManager::new(&store);
    mgr.delete(&name)?;
    println!("  {} Dataset '{}' deleted.", "✓".green(), name);
    Ok(())
}

fn cmd_eval_evaluator_create(name: String, evaluator_type: String, config: Option<String>, description: String) -> Result<()> {
    if !EvaluatorRegistry::is_valid_type(&evaluator_type) {
        bail!(
            "Unknown evaluator type '{}'. Valid types: {}",
            evaluator_type,
            EvaluatorRegistry::builtin_types().join(", ")
        );
    }

    let store = Store::open_default()?;

    // Store config in blob store if provided
    let config_blob = match config {
        Some(ref c) => {
            // Try to parse as JSON first
            let config_val: serde_json::Value = if c.starts_with('{') || c.starts_with('[') {
                serde_json::from_str(c)
                    .map_err(|e| anyhow::anyhow!("Invalid config JSON: {}", e))?
            } else if std::path::Path::new(c).exists() {
                // If it's a file path, read and parse it
                let content = std::fs::read_to_string(c)?;
                serde_json::from_str(&content)?
            } else {
                // Treat as a simple string value based on type
                match evaluator_type.as_str() {
                    "contains" => serde_json::json!({"substring": c}),
                    "regex" => serde_json::json!({"pattern": c}),
                    "custom" => serde_json::json!({"command": c}),
                    "llm_judge" => serde_json::json!({"criteria": c}),
                    _ => serde_json::json!({"value": c}),
                }
            };
            store.blobs.put_json(&config_val)?
        }
        None => String::new(),
    };

    let evaluator = rewind_store::Evaluator::new(&name, &evaluator_type, &config_blob, &description);
    store.create_evaluator(&evaluator)?;

    println!("{}", "⏪ Rewind — Evaluator Created".cyan().bold());
    println!();
    println!("  {} {}", "Name:".dimmed(), evaluator.name.white().bold());
    println!("  {} {}", "Type:".dimmed(), evaluator_type.cyan());
    if config.is_some() {
        println!("  {} {}", "Config:".dimmed(), "stored".dimmed());
    }
    println!();
    Ok(())
}

fn cmd_eval_evaluator_list() -> Result<()> {
    let store = Store::open_default()?;
    let evaluators = store.list_evaluators()?;

    if evaluators.is_empty() {
        println!("{}", "No evaluators yet.".dimmed());
        println!("  Create one: {}", "rewind eval evaluator create my-eval -t exact_match".green());
        return Ok(());
    }

    println!("{}", "⏪ Rewind — Evaluators".cyan().bold());
    println!();
    println!(
        "  {:>20} {:>15} {}",
        "NAME".dimmed(), "TYPE".dimmed(), "DESCRIPTION".dimmed(),
    );
    println!("  {}", "─".repeat(55).dimmed());

    for ev in &evaluators {
        println!(
            "  {:>20} {:>15} {}",
            ev.name.white().bold(),
            ev.evaluator_type.cyan(),
            ev.description.dimmed(),
        );
    }
    println!();
    Ok(())
}

fn cmd_eval_evaluator_delete(name: String) -> Result<()> {
    let store = Store::open_default()?;
    store.delete_evaluator(&name)?;
    println!("  {} Evaluator '{}' deleted.", "✓".green(), name);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_eval_run(
    dataset_ref: String,
    command: String,
    evaluator_names: Vec<String>,
    name: Option<String>,
    fail_below: Option<f64>,
    timeout: u64,
    json_output: bool,
    metadata_str: String,
) -> Result<()> {
    if evaluator_names.is_empty() {
        bail!("At least one evaluator is required. Use -e <name> to specify.");
    }

    let store = Store::open_default()?;
    let (ds_name, ds_version) = rewind_eval::dataset::parse_dataset_ref(&dataset_ref);

    let metadata: serde_json::Value = serde_json::from_str(&metadata_str)
        .unwrap_or(serde_json::json!({}));

    let runner = ExperimentRunner::new(&store);
    let config = RunConfig {
        dataset_name: ds_name.to_string(),
        dataset_version: ds_version,
        evaluator_names: evaluator_names.clone(),
        command: command.clone(),
        name,
        fail_below,
        timeout_per_example_secs: timeout,
        metadata,
    };

    if !json_output {
        println!("{}", "⏪ Rewind — Running Experiment".cyan().bold());
        println!();
        println!("  {} {}", "Dataset:".dimmed(), ds_name.white().bold());
        println!("  {} {}", "Command:".dimmed(), command.cyan());
        println!("  {} {}", "Evaluators:".dimmed(), evaluator_names.join(", ").yellow());
        if let Some(threshold) = fail_below {
            println!("  {} {}", "Fail below:".dimmed(), format!("{:.1}%", threshold * 100.0).red());
        }
        println!();
    }

    let experiment = runner.run(config)?;

    if json_output {
        let output = serde_json::json!({
            "schema_version": 1,
            "experiment_id": experiment.id,
            "experiment_name": experiment.name,
            "dataset_version": experiment.dataset_version,
            "status": experiment.status.as_str(),
            "total_examples": experiment.total_examples,
            "avg_score": experiment.avg_score,
            "min_score": experiment.min_score,
            "max_score": experiment.max_score,
            "pass_rate": experiment.pass_rate,
            "total_duration_ms": experiment.total_duration_ms,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let avg = experiment.avg_score.unwrap_or(0.0);
        let pass_rate = experiment.pass_rate.unwrap_or(0.0);

        println!("  {} {}", "Experiment:".dimmed(), experiment.name.white().bold());
        println!("  {} {}", "Status:".dimmed(), experiment.status.as_str().green());
        println!("  {} {}", "Examples:".dimmed(), experiment.total_examples.to_string().yellow());
        println!(
            "  {} {}",
            "Avg Score:".dimmed(),
            format_score(avg),
        );
        println!(
            "  {} {}",
            "Pass Rate:".dimmed(),
            format!("{:.1}%", pass_rate * 100.0).white().bold(),
        );
        println!(
            "  {} {}",
            "Duration:".dimmed(),
            format!("{}ms", experiment.total_duration_ms).dimmed(),
        );
        println!();

        // Show per-example results
        let results = store.get_experiment_results(&experiment.id)?;
        for result in &results {
            let scores = store.get_experiment_scores(&result.id)?;
            let avg_score = if scores.is_empty() {
                0.0
            } else {
                scores.iter().map(|s| s.score).sum::<f64>() / scores.len() as f64
            };

            let status_icon = match result.status.as_str() {
                "success" => "✓".green(),
                "error" => "✗".red(),
                _ => "…".yellow(),
            };

            let score_details: Vec<String> = scores
                .iter()
                .map(|s| {
                    let ev = store.get_evaluator_by_name("").ok().flatten(); // We'll use the id
                    let _ = ev;
                    format!("{:.2}", s.score)
                })
                .collect();

            println!(
                "  {} #{:<3} {} {}",
                status_icon,
                result.ordinal,
                format_score(avg_score),
                if let Some(ref err) = result.error {
                    err.red().to_string()
                } else {
                    score_details.join(" | ").dimmed().to_string()
                },
            );
        }
        println!();
    }

    // Check fail_below threshold
    if let Some(threshold) = fail_below {
        let avg = experiment.avg_score.unwrap_or(0.0);
        if avg < threshold {
            if !json_output {
                println!(
                    "  {} avg_score {:.3} < threshold {:.3}",
                    "FAILED:".red().bold(),
                    avg,
                    threshold,
                );
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

fn cmd_eval_compare(left_ref: String, right_ref: String, json_output: bool, force: bool) -> Result<()> {
    let store = Store::open_default()?;

    // Resolve experiment references by name or ID
    let left = resolve_experiment(&store, &left_ref)?;
    let right = resolve_experiment(&store, &right_ref)?;

    let comparison = rewind_eval::compare_experiments(&store, &left.id, &right.id, force)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
        return Ok(());
    }

    println!("{}", "⏪ Rewind — Experiment Comparison".cyan().bold());
    println!();
    println!(
        "  {} {} (avg: {}) vs {} (avg: {})",
        "Comparing:".dimmed(),
        comparison.left_name.cyan().bold(),
        format_score(comparison.left_avg_score),
        comparison.right_name.yellow().bold(),
        format_score(comparison.right_avg_score),
    );
    println!();

    let delta_str = if comparison.overall_delta > 0.0 {
        format!("+{:.3}", comparison.overall_delta).green().bold().to_string()
    } else if comparison.overall_delta < 0.0 {
        format!("{:.3}", comparison.overall_delta).red().bold().to_string()
    } else {
        "0.000".dimmed().to_string()
    };
    println!("  {} {}", "Overall delta:".dimmed(), delta_str);
    println!(
        "  {} {} regressions, {} improvements, {} unchanged",
        "Summary:".dimmed(),
        comparison.regressions.to_string().red(),
        comparison.improvements.to_string().green(),
        comparison.unchanged.to_string().dimmed(),
    );
    println!();

    // Show per-example diffs (only changes)
    let changes: Vec<_> = comparison
        .example_diffs
        .iter()
        .filter(|d| d.direction != rewind_eval::DiffDirection::Unchanged)
        .collect();

    if !changes.is_empty() {
        println!("  {}", "Changes:".dimmed());
        for diff in changes {
            let icon = match diff.direction {
                rewind_eval::DiffDirection::Regression => "▼".red(),
                rewind_eval::DiffDirection::Improvement => "▲".green(),
                rewind_eval::DiffDirection::Unchanged => "═".dimmed(),
            };
            let delta = if diff.delta > 0.0 {
                format!("+{:.3}", diff.delta).green().to_string()
            } else {
                format!("{:.3}", diff.delta).red().to_string()
            };
            println!(
                "    {} #{:<3} {:.2} → {:.2} ({}) {}",
                icon,
                diff.ordinal,
                diff.left_score,
                diff.right_score,
                delta,
                diff.input_preview.dimmed(),
            );
        }
    } else {
        println!("  {}", "No changes detected.".dimmed());
    }

    println!();
    Ok(())
}

fn cmd_eval_experiments(dataset_filter: Option<String>) -> Result<()> {
    let store = Store::open_default()?;
    let experiments = match dataset_filter {
        Some(ref name) => store.list_experiments_by_dataset(name)?,
        None => store.list_experiments()?,
    };

    if experiments.is_empty() {
        println!("{}", "No experiments yet.".dimmed());
        println!("  Run one: {}", "rewind eval run <dataset> -c <command> -e <evaluator>".green());
        return Ok(());
    }

    println!("{}", "⏪ Rewind — Experiments".cyan().bold());
    println!();
    println!(
        "  {:>25} {:>10} {:>8} {:>10} {:>10} {}",
        "NAME".dimmed(), "STATUS".dimmed(), "EXAMPLES".dimmed(), "AVG SCORE".dimmed(), "PASS RATE".dimmed(), "CREATED".dimmed(),
    );
    println!("  {}", "─".repeat(80).dimmed());

    for exp in &experiments {
        let status_str = match exp.status {
            rewind_store::ExperimentStatus::Completed => exp.status.as_str().green(),
            rewind_store::ExperimentStatus::Failed => exp.status.as_str().red(),
            rewind_store::ExperimentStatus::Running => exp.status.as_str().yellow(),
            _ => exp.status.as_str().dimmed(),
        };
        let ago = format_time_ago(exp.created_at);
        println!(
            "  {:>25} {:>10} {:>8} {:>10} {:>10} {}",
            exp.name.white().bold(),
            status_str,
            exp.total_examples.to_string().yellow(),
            exp.avg_score.map(|s| format!("{:.3}", s)).unwrap_or("-".to_string()),
            exp.pass_rate.map(|r| format!("{:.0}%", r * 100.0)).unwrap_or("-".to_string()),
            ago.dimmed(),
        );
    }
    println!();
    Ok(())
}

fn cmd_eval_show(experiment_ref: String) -> Result<()> {
    let store = Store::open_default()?;
    let experiment = resolve_experiment(&store, &experiment_ref)?;

    println!("{}", "⏪ Rewind — Experiment Detail".cyan().bold());
    println!();
    println!("  {} {}", "Name:".dimmed(), experiment.name.white().bold());
    println!("  {} {}", "ID:".dimmed(), experiment.id.dimmed());
    println!("  {} {}", "Status:".dimmed(), experiment.status.as_str().green());
    println!("  {} {}", "Dataset version:".dimmed(), format!("v{}", experiment.dataset_version).yellow());
    println!("  {} {}", "Examples:".dimmed(), experiment.total_examples.to_string().yellow());
    println!(
        "  {} {}",
        "Avg Score:".dimmed(),
        format_score(experiment.avg_score.unwrap_or(0.0)),
    );
    println!(
        "  {} {}",
        "Pass Rate:".dimmed(),
        experiment.pass_rate.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or("-".to_string()).white().bold(),
    );
    println!(
        "  {} avg={:.3} / min={:.3} / max={:.3}",
        "Scores:".dimmed(),
        experiment.avg_score.unwrap_or(0.0),
        experiment.min_score.unwrap_or(0.0),
        experiment.max_score.unwrap_or(0.0),
    );
    println!("  {} {}", "Duration:".dimmed(), format!("{}ms", experiment.total_duration_ms).dimmed());
    println!();

    // Show per-example results
    let results = store.get_experiment_results(&experiment.id)?;
    println!("  {}", "Results:".dimmed());
    for result in &results {
        let scores = store.get_experiment_scores(&result.id)?;
        let status_icon = match result.status.as_str() {
            "success" => "✓".green(),
            "error" => "✗".red(),
            _ => "…".yellow(),
        };

        let score_strs: Vec<String> = scores.iter().map(|s| format!("{:.2}", s.score)).collect();
        let avg = if scores.is_empty() {
            0.0
        } else {
            scores.iter().map(|s| s.score).sum::<f64>() / scores.len() as f64
        };

        println!(
            "    {} #{:<3} {} {}  {}",
            status_icon,
            result.ordinal,
            format_score(avg),
            format!("({}ms)", result.duration_ms).dimmed(),
            if let Some(ref err) = result.error {
                err.red().to_string()
            } else {
                score_strs.join(" | ").dimmed().to_string()
            },
        );

        // Show reasoning for failed scores
        for s in &scores {
            if !s.passed && !s.reasoning.is_empty() {
                let reason: String = s.reasoning.chars().take(80).collect();
                println!("          {} {}", "↳".dimmed(), reason.dimmed());
            }
        }
    }
    println!();
    Ok(())
}

fn cmd_eval_score(
    session_ref: String,
    evaluator_names: Vec<String>,
    timeline_ref: Option<String>,
    compare_timelines: bool,
    expected_json: Option<String>,
    json_output: bool,
    force: bool,
) -> Result<()> {
    if evaluator_names.is_empty() {
        bail!("At least one evaluator is required. Use -e <name> to specify.");
    }

    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let all_timelines = store.get_timelines(&session.id)?;

    if all_timelines.is_empty() {
        bail!("Session '{}' has no timelines — nothing to score", session.name);
    }

    // Parse expected value if provided
    let expected: serde_json::Value = match expected_json {
        Some(ref s) => serde_json::from_str(s)
            .map_err(|e| anyhow::anyhow!("Invalid --expected JSON: {}", e))?,
        None => serde_json::Value::Null,
    };

    // Determine which timelines to score
    let timelines_to_score: Vec<&rewind_store::Timeline> = if compare_timelines {
        all_timelines.iter().collect()
    } else {
        let tl_id = match timeline_ref {
            Some(ref r) => resolve_timeline_ref(&all_timelines, r)?,
            None => {
                // Default to "main" timeline
                all_timelines
                    .iter()
                    .find(|t| t.label == "main")
                    .or_else(|| all_timelines.first())
                    .map(|t| t.id.clone())
                    .context("No timelines found")?
            }
        };
        all_timelines
            .iter()
            .filter(|t| t.id == tl_id)
            .collect()
    };

    let registry = EvaluatorRegistry::new(&store);

    // Score each timeline × evaluator
    struct TimelineResult {
        label: String,
        id: String,
        scores: Vec<EvalResult>,
    }
    struct EvalResult {
        name: String,
        score: f64,
        #[allow(dead_code)]
        passed: bool,
        #[allow(dead_code)]
        reasoning: String,
    }
    let mut results: Vec<TimelineResult> = Vec::new();

    for tl in &timelines_to_score {
        let (input, output) = extract_timeline_output(&store, &tl.id)?;
        let mut scores_for_tl: Vec<EvalResult> = Vec::new();

        for eval_name in &evaluator_names {
            // Check cache: if we already scored this timeline+evaluator, reuse it
            let evaluator = store
                .get_evaluator_by_name(eval_name)?
                .ok_or_else(|| anyhow::anyhow!("Evaluator '{}' not found", eval_name))?;

            if !force
                && let Some(cached) = store.get_timeline_score(&tl.id, &evaluator.id)?
            {
                scores_for_tl.push(EvalResult {
                    name: eval_name.clone(),
                    score: cached.score,
                    passed: cached.passed,
                    reasoning: cached.reasoning.clone(),
                });
                continue;
            }

            // Run the evaluator
            let (evaluator_id, score_result) =
                registry.score(eval_name, &input, &output, &expected)?;

            // Persist the score
            let timeline_score = rewind_store::TimelineScore::new(
                &session.id,
                &tl.id,
                &evaluator_id,
                score_result.score,
                score_result.passed,
                &score_result.reasoning,
                &input.to_string(),
                &output.to_string(),
            );
            store.create_timeline_score(&timeline_score)?;

            scores_for_tl.push(EvalResult {
                name: eval_name.clone(),
                score: score_result.score,
                passed: score_result.passed,
                reasoning: score_result.reasoning.clone(),
            });
        }

        results.push(TimelineResult {
            label: tl.label.clone(),
            id: tl.id.clone(),
            scores: scores_for_tl,
        });
    }

    // Output
    fn avg_score(scores: &[EvalResult]) -> f64 {
        if scores.is_empty() {
            0.0
        } else {
            scores.iter().map(|s| s.score).sum::<f64>() / scores.len() as f64
        }
    }

    if json_output {
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|tl| {
                let score_map: serde_json::Map<String, serde_json::Value> = tl.scores
                    .iter()
                    .map(|s| {
                        (
                            s.name.clone(),
                            serde_json::json!({
                                "score": s.score,
                                "passed": s.passed,
                                "reasoning": s.reasoning,
                            }),
                        )
                    })
                    .collect();
                serde_json::json!({
                    "timeline_id": tl.id,
                    "timeline_label": tl.label,
                    "scores": score_map,
                    "avg_score": avg_score(&tl.scores),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
        return Ok(());
    }

    // Pretty-print table
    println!("{}", "⏪ Rewind — Timeline Scores".cyan().bold());
    println!();
    println!("  {} {}", "Session:".dimmed(), session.name.white().bold());
    println!(
        "  {} {}",
        "Evaluators:".dimmed(),
        evaluator_names.join(", ").cyan()
    );
    println!();

    // Header row
    let label_width = results.iter().map(|tl| tl.label.len()).max().unwrap_or(8).max(10);
    let col_width = evaluator_names.iter().map(|n| n.len()).max().unwrap_or(8).max(8);

    print!("  {:<width$}", "Timeline", width = label_width + 2);
    for name in &evaluator_names {
        print!("  {:>width$}", name, width = col_width);
    }
    println!("  {:>6}", "avg");

    print!("  {}", "─".repeat(label_width + 2));
    for _ in &evaluator_names {
        print!("  {}", "─".repeat(col_width));
    }
    println!("  {}", "─".repeat(6));

    // Data rows
    for tl in &results {
        print!("  {:<width$}", tl.label.white().bold(), width = label_width + 2);
        let avg = avg_score(&tl.scores);
        for s in &tl.scores {
            print!("  {:>width$}", format_score(s.score), width = col_width);
        }
        println!("  {:>6}", format_score(avg));
    }

    // Delta lines: compare each fork against main (first timeline)
    if results.len() > 1 {
        println!();
        let main_tl = results.first().unwrap();
        let main_avg = avg_score(&main_tl.scores);

        for fork_tl in results.iter().skip(1) {
            let fork_avg = avg_score(&fork_tl.scores);
            let delta = fork_avg - main_avg;
            let delta_str = if delta >= 0.0 {
                format!("+{:.2}", delta).green()
            } else {
                format!("{:.2}", delta).red()
            };
            print!(
                "  Delta ({} vs {}): {} avg",
                fork_tl.label,
                main_tl.label,
                delta_str,
            );
            if delta > 0.01 {
                println!("  {}", "✓".green());
            } else if delta < -0.01 {
                println!("  {}", "✗".red());
            } else {
                println!("  {}", "─".yellow());
            }
        }
    }

    println!();
    Ok(())
}

fn resolve_experiment(store: &Store, reference: &str) -> Result<rewind_store::Experiment> {
    // Try by name first, then by ID, then by ID prefix
    if let Some(exp) = store.get_experiment_by_name(reference)? {
        return Ok(exp);
    }
    if let Some(exp) = store.get_experiment(reference)? {
        return Ok(exp);
    }
    // Try prefix
    let experiments = store.list_experiments()?;
    experiments
        .into_iter()
        .find(|e| e.id.starts_with(reference))
        .context(format!("Experiment not found: {}", reference))
}

fn format_score(score: f64) -> colored::ColoredString {
    let s = format!("{:.3}", score);
    if score >= 0.8 {
        s.green().bold()
    } else if score >= 0.6 {
        s.yellow()
    } else {
        s.red()
    }
}

// ── Hooks commands ──────────────────────────────────────────

const HOOK_SCRIPT: &str = include_str!("../../../assets/claude-code-hook.sh");

const HOOK_EVENT_TYPES: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SessionStart",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
    "UserPromptSubmit",
    "Stop",
];

/// Event types that should match all tools (matcher = "*")
const TOOL_EVENT_TYPES: &[&str] = &["PreToolUse", "PostToolUse", "PostToolUseFailure"];

fn get_home_dir() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .context("HOME environment variable not set")
}

fn hook_script_path() -> Result<std::path::PathBuf> {
    Ok(get_home_dir()?.join(".rewind/hooks/claude-code-hook.sh"))
}

fn claude_settings_path() -> Result<std::path::PathBuf> {
    Ok(get_home_dir()?.join(".claude/settings.json"))
}

fn read_claude_settings() -> Result<serde_json::Value> {
    let path = claude_settings_path()?;
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .context("Failed to read ~/.claude/settings.json")?;
        serde_json::from_str(&content)
            .context("Failed to parse ~/.claude/settings.json")
    } else {
        Ok(serde_json::json!({}))
    }
}

fn write_claude_settings(settings: &serde_json::Value) -> Result<()> {
    let path = claude_settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, content)
        .context("Failed to write ~/.claude/settings.json")
}

fn make_hook_entry(matcher: &str) -> serde_json::Value {
    serde_json::json!({
        "matcher": matcher,
        "hooks": [
            {
                "type": "command",
                "command": "bash ~/.rewind/hooks/claude-code-hook.sh"
            }
        ]
    })
}

fn is_rewind_hook(entry: &serde_json::Value) -> bool {
    if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
        hooks.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .map(|c| c.contains("claude-code-hook.sh"))
                .unwrap_or(false)
        })
    } else {
        false
    }
}

async fn cmd_hooks_install(port: u16) -> Result<()> {
    println!("{}", "⏪ Rewind — Installing Claude Code Hooks".cyan().bold());
    println!();

    // 1. Write hook script
    let script_path = hook_script_path()?;
    if let Some(parent) = script_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write script with REWIND_PORT baked in if non-default
    let script_content = if port != 4800 {
        HOOK_SCRIPT.replace(
            "REWIND_PORT=\"${REWIND_PORT:-4800}\"",
            &format!("REWIND_PORT=\"${{REWIND_PORT:-{}}}\"", port),
        )
    } else {
        HOOK_SCRIPT.to_string()
    };

    std::fs::write(&script_path, script_content)?;

    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
    }

    println!("  {} Hook script written to {}", "✓".green(), script_path.display().to_string().dimmed());

    // 2. Read Claude settings
    let mut settings = read_claude_settings()?;

    // Ensure "hooks" object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }

    let hooks_obj = settings["hooks"].as_object_mut()
        .context("hooks field in settings.json is not an object")?;

    // 3. For each event type, add our hook entry (alongside existing ones)
    for event_type in HOOK_EVENT_TYPES {
        let matcher = if TOOL_EVENT_TYPES.contains(event_type) { "*" } else { "" };
        let new_entry = make_hook_entry(matcher);

        if let Some(existing) = hooks_obj.get_mut(*event_type) {
            if let Some(arr) = existing.as_array_mut() {
                // Remove any existing Rewind hooks first (idempotent re-install)
                arr.retain(|entry| !is_rewind_hook(entry));
                // Append our new hook
                arr.push(new_entry);
            } else {
                // Not an array — wrap existing + add ours
                let old = existing.clone();
                *existing = serde_json::json!([old, new_entry]);
            }
        } else {
            // No existing hooks for this event type
            hooks_obj.insert(event_type.to_string(), serde_json::json!([new_entry]));
        }
    }

    // 4. Write back
    write_claude_settings(&settings)?;

    println!("  {} Claude Code settings updated at {}", "✓".green(), claude_settings_path()?.display().to_string().dimmed());
    println!("  {} {} hook event types configured", "✓".green(), HOOK_EVENT_TYPES.len().to_string().yellow());
    println!();
    println!(
        "  {} Start the server with {} to begin observing Claude Code sessions.",
        "→".cyan(),
        "rewind web".green().bold(),
    );
    println!();

    Ok(())
}

fn cmd_hooks_uninstall() -> Result<()> {
    println!("{}", "⏪ Rewind — Uninstalling Claude Code Hooks".cyan().bold());
    println!();

    // 1. Read Claude settings
    let mut settings = read_claude_settings()?;

    if let Some(hooks_obj) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        let mut removed_count = 0u32;

        for event_type in HOOK_EVENT_TYPES {
            if let Some(existing) = hooks_obj.get_mut(*event_type)
                && let Some(arr) = existing.as_array_mut() {
                let before = arr.len();
                arr.retain(|entry| !is_rewind_hook(entry));
                removed_count += (before - arr.len()) as u32;
            }
        }

        // Clean up empty arrays
        let empty_keys: Vec<String> = hooks_obj
            .iter()
            .filter(|(_, v)| v.as_array().map(|a| a.is_empty()).unwrap_or(false))
            .map(|(k, _)| k.clone())
            .collect();
        for key in empty_keys {
            hooks_obj.remove(&key);
        }

        // Write back
        write_claude_settings(&settings)?;

        if removed_count > 0 {
            println!("  {} Removed {} Rewind hook entries from Claude Code settings.", "✓".green(), removed_count.to_string().yellow());
        } else {
            println!("  {} No Rewind hooks found in Claude Code settings.", "○".dimmed());
        }
    } else {
        println!("  {} No hooks section found in Claude Code settings.", "○".dimmed());
    }

    // 2. Remove hook script if it exists
    let script_path = hook_script_path()?;
    if script_path.exists() {
        std::fs::remove_file(&script_path)?;
        println!("  {} Removed hook script at {}", "✓".green(), script_path.display().to_string().dimmed());
    }

    println!();
    Ok(())
}

async fn cmd_hooks_status(port: u16) -> Result<()> {
    println!("{}", "⏪ Rewind — Hooks Status".cyan().bold());
    println!();

    // 1. Check if hooks are installed in settings.json
    let settings = read_claude_settings()?;
    let hooks_installed = if let Some(hooks_obj) = settings.get("hooks").and_then(|h| h.as_object()) {
        let mut installed_events = Vec::new();
        for event_type in HOOK_EVENT_TYPES {
            if let Some(arr) = hooks_obj.get(*event_type).and_then(|v| v.as_array())
                && arr.iter().any(is_rewind_hook) {
                installed_events.push(*event_type);
            }
        }
        if installed_events.is_empty() {
            println!("  {} {}", "Hooks:".dimmed(), "not installed".red());
            false
        } else {
            println!(
                "  {} {} ({}/{} event types)",
                "Hooks:".dimmed(),
                "installed".green().bold(),
                installed_events.len().to_string().yellow(),
                HOOK_EVENT_TYPES.len().to_string().yellow(),
            );
            true
        }
    } else {
        println!("  {} {}", "Hooks:".dimmed(), "not installed".red());
        false
    };

    // 2. Check hook script
    let script_path = hook_script_path()?;
    if script_path.exists() {
        println!("  {} {}", "Script:".dimmed(), script_path.display().to_string().green());
    } else {
        println!("  {} {}", "Script:".dimmed(), "not found".red());
    }

    // 3. Check if Rewind server is reachable
    let server_url = format!("http://127.0.0.1:{}/api/health", port);
    let server_running = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => {
            matches!(client.get(&server_url).send().await, Ok(resp) if resp.status().is_success())
        }
        Err(_) => false,
    };

    if server_running {
        println!(
            "  {} {} (port {})",
            "Server:".dimmed(),
            "running".green().bold(),
            port.to_string().yellow(),
        );
    } else {
        println!(
            "  {} {} (port {})",
            "Server:".dimmed(),
            "not running".red(),
            port.to_string().yellow(),
        );
    }

    // 4. Check for buffered events
    let buffer_path = get_home_dir()?.join(".rewind/hooks/buffer.jsonl");
    if buffer_path.exists() {
        let content = std::fs::read_to_string(&buffer_path).unwrap_or_default();
        let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
        if line_count > 0 {
            println!(
                "  {} {} buffered events in {}",
                "Buffer:".dimmed(),
                line_count.to_string().yellow().bold(),
                buffer_path.display().to_string().dimmed(),
            );
        } else {
            println!("  {} {}", "Buffer:".dimmed(), "empty".dimmed());
        }
    } else {
        println!("  {} {}", "Buffer:".dimmed(), "no buffered events".dimmed());
    }

    // 5. Warnings
    println!();
    if hooks_installed && !server_running {
        println!(
            "  {} Hooks are installed but the server is not running.",
            "WARNING:".yellow().bold(),
        );
        println!(
            "  Events will be buffered locally until the server starts.",
        );
        println!(
            "  Start it with: {}",
            format!("rewind web --port {}", port).green(),
        );
        println!();
    } else if !hooks_installed {
        println!(
            "  Install hooks with: {}",
            "rewind hooks install".green(),
        );
        println!();
    }

    Ok(())
}

// ── Demo data seeding ─────────────────────────────────────────

fn seed_demo_data(store: &Store) -> Result<()> {
    use rewind_store::*;

    let session = Session::new("research-agent-demo");
    let timeline = Timeline::new_root(&session.id);

    store.create_session(&session)?;
    store.create_timeline(&timeline)?;

    // Step 1: System prompt + user query
    let req1 = serde_json::json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a research assistant. When asked about a topic, use the provided tools to search for information and synthesize an accurate answer with citations."},
            {"role": "user", "content": "What is the current population of Tokyo, and how has it changed over the last decade?"}
        ],
        "tools": [
            {"type": "function", "function": {"name": "web_search", "description": "Search the web for information", "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}}
        ]
    });
    let resp1 = serde_json::json!({
        "id": "chatcmpl-001",
        "model": "gpt-4o-2024-08-06",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo current population 2024\"}"}}]}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 156, "completion_tokens": 28, "total_tokens": 184}
    });

    create_step_with_blobs(store, &timeline.id, &session.id, 1, StepType::LlmCall, StepStatus::Success,
        "gpt-4o", 320, 156, 28, &req1, &resp1, None)?;

    // Step 2: Tool result — web search
    let req2 = serde_json::json!({
        "role": "tool",
        "tool_call_id": "call_1",
        "content": "Tokyo metropolitan area population (2024): approximately 13.96 million in the 23 special wards, 37.4 million in the Greater Tokyo Area. The population of the 23 wards peaked in 2020 at 14.04 million before a slight decline attributed to COVID-19 migration patterns. Source: Tokyo Metropolitan Government Statistics Bureau."
    });
    let resp2 = serde_json::json!({"status": "delivered"});

    create_step_with_blobs(store, &timeline.id, &session.id, 2, StepType::ToolResult, StepStatus::Success,
        "tool", 45, 0, 0, &req2, &resp2, None)?;

    // Step 3: Second LLM call — agent processes search results
    let req3 = serde_json::json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a research assistant. When asked about a topic, use the provided tools to search for information and synthesize an accurate answer with citations."},
            {"role": "user", "content": "What is the current population of Tokyo, and how has it changed over the last decade?"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo current population 2024\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "Tokyo metropolitan area population (2024): approximately 13.96 million in the 23 special wards, 37.4 million in the Greater Tokyo Area. The population of the 23 wards peaked in 2020 at 14.04 million before a slight decline attributed to COVID-19 migration patterns. Source: Tokyo Metropolitan Government Statistics Bureau."},
        ],
        "tools": [
            {"type": "function", "function": {"name": "web_search", "description": "Search the web for information", "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}}
        ]
    });
    let resp3 = serde_json::json!({
        "id": "chatcmpl-002",
        "model": "gpt-4o-2024-08-06",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": [{"id": "call_2", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo population change 2014 to 2024 decade trend\"}"}}]}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 312, "completion_tokens": 35, "total_tokens": 347}
    });

    create_step_with_blobs(store, &timeline.id, &session.id, 3, StepType::LlmCall, StepStatus::Success,
        "gpt-4o", 890, 312, 35, &req3, &resp3, None)?;

    // Step 4: Tool result — search about decade trend (THIS HAS MISLEADING DATA)
    let req4 = serde_json::json!({
        "role": "tool",
        "tool_call_id": "call_2",
        "content": "ERROR: Search API rate limited. Cached result returned from 2019 dataset. Tokyo population trend 2014-2019: steady growth from 13.35M to 13.96M in 23 wards (+4.6%). National Institute of Population projections (2019): expected continued growth through 2025, reaching 14.2M. Note: this data predates COVID-19 impacts."
    });
    let resp4 = serde_json::json!({"status": "delivered"});

    create_step_with_blobs(store, &timeline.id, &session.id, 4, StepType::ToolResult, StepStatus::Success,
        "tool", 38, 0, 0, &req4, &resp4, None)?;

    // Step 5: Third LLM call — agent tries to synthesize (BUT context now has contradictory data)
    let req5 = serde_json::json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a research assistant. When asked about a topic, use the provided tools to search for information and synthesize an accurate answer with citations."},
            {"role": "user", "content": "What is the current population of Tokyo, and how has it changed over the last decade?"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo current population 2024\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "Tokyo metropolitan area population (2024): approximately 13.96 million in the 23 special wards, 37.4 million in the Greater Tokyo Area. The population of the 23 wards peaked in 2020 at 14.04 million before a slight decline attributed to COVID-19 migration patterns. Source: Tokyo Metropolitan Government Statistics Bureau."},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_2", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo population change 2014 to 2024 decade trend\"}"}}]},
            {"role": "tool", "tool_call_id": "call_2", "content": "ERROR: Search API rate limited. Cached result returned from 2019 dataset. Tokyo population trend 2014-2019: steady growth from 13.35M to 13.96M in 23 wards (+4.6%). National Institute of Population projections (2019): expected continued growth through 2025, reaching 14.2M. Note: this data predates COVID-19 impacts."},
        ],
        "tools": [
            {"type": "function", "function": {"name": "web_search", "description": "Search the web for information", "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}}
        ]
    });
    // The agent hallucinates — it combines the stale 2019 projection with current data incorrectly
    let resp5 = serde_json::json!({
        "id": "chatcmpl-003",
        "model": "gpt-4o-2024-08-06",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "# Tokyo Population Analysis\n\n## Current Population\nTokyo's 23 special wards currently have a population of approximately **14.2 million** (2024), with the Greater Tokyo Area home to 37.4 million people.\n\n## Decade Trend (2014-2024)\nOver the past decade, Tokyo has experienced **steady, uninterrupted growth**:\n- 2014: 13.35 million\n- 2019: 13.96 million\n- 2024: 14.2 million (projected, confirmed by current data)\n\nThis represents a **+6.4% increase** over the decade, driven primarily by domestic migration from rural prefectures and steady international immigration.\n\n## Sources\n- Tokyo Metropolitan Government Statistics Bureau (2024)\n- National Institute of Population and Social Security Research (2019 projections)\n\n*Note: Tokyo's growth has been remarkably consistent, with no significant disruptions observed during this period.*"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 520, "completion_tokens": 180, "total_tokens": 700}
    });

    create_step_with_blobs(store, &timeline.id, &session.id, 5, StepType::LlmCall, StepStatus::Error,
        "gpt-4o", 1450, 520, 180, &req5, &resp5,
        Some("HALLUCINATION: Agent used stale 2019 projection (14.2M) as current fact, ignored COVID-19 dip to 13.96M, and claimed 'no significant disruptions' despite search result explicitly noting COVID impacts."))?;

    // ── Span tree: give the demo a multi-agent feel ──────────────
    // Root span: supervisor agent
    let mut supervisor = Span::new(&session.id, &timeline.id, SpanType::Agent, "supervisor");
    supervisor.status = "error".to_string();
    supervisor.duration_ms = 2743;
    supervisor.ended_at = Some(supervisor.started_at + chrono::Duration::milliseconds(2743));
    store.create_span(&supervisor)?;

    // Child span: researcher agent (steps 1-4)
    let mut researcher = Span::new(&session.id, &timeline.id, SpanType::Agent, "researcher");
    researcher.parent_span_id = Some(supervisor.id.clone());
    researcher.status = "completed".to_string();
    researcher.duration_ms = 1293;
    researcher.ended_at = Some(researcher.started_at + chrono::Duration::milliseconds(1293));
    store.create_span(&researcher)?;

    // Child span: web_search tool (step 2)
    let mut tool1 = Span::new(&session.id, &timeline.id, SpanType::Tool, "web_search");
    tool1.parent_span_id = Some(researcher.id.clone());
    tool1.status = "completed".to_string();
    tool1.duration_ms = 45;
    tool1.ended_at = Some(tool1.started_at + chrono::Duration::milliseconds(45));
    store.create_span(&tool1)?;

    // Child span: web_search tool (step 4)
    let mut tool2 = Span::new(&session.id, &timeline.id, SpanType::Tool, "web_search");
    tool2.parent_span_id = Some(researcher.id.clone());
    tool2.status = "completed".to_string();
    tool2.duration_ms = 38;
    tool2.ended_at = Some(tool2.started_at + chrono::Duration::milliseconds(38));
    store.create_span(&tool2)?;

    // Child span: writer agent (step 5)
    let mut writer = Span::new(&session.id, &timeline.id, SpanType::Agent, "writer");
    writer.parent_span_id = Some(supervisor.id.clone());
    writer.status = "error".to_string();
    writer.duration_ms = 1450;
    writer.ended_at = Some(writer.started_at + chrono::Duration::milliseconds(1450));
    writer.error = Some("Hallucination — used stale 2019 projection as current fact".to_string());
    store.create_span(&writer)?;

    // Link steps to spans
    let steps = store.get_steps(&timeline.id)?;
    for step in &steps {
        match step.step_number {
            1 => store.update_step_span_id(&step.id, &researcher.id)?,
            2 => store.update_step_span_id(&step.id, &tool1.id)?,
            3 => store.update_step_span_id(&step.id, &researcher.id)?,
            4 => store.update_step_span_id(&step.id, &tool2.id)?,
            5 => store.update_step_span_id(&step.id, &writer.id)?,
            _ => {}
        }
    }

    // Update session totals
    store.update_session_stats(
        &session.id,
        5,
        184 + 347 + 700,
    )?;
    store.update_session_status(&session.id, SessionStatus::Failed)?;

    // Now create a FORKED timeline showing the fix
    let fork = Timeline::new_fork(&session.id, &timeline.id, 4, "fixed");
    store.create_timeline(&fork)?;

    // Fork step 5: Same request but with corrected tool result in context
    let req5_fixed = serde_json::json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a research assistant. When asked about a topic, use the provided tools to search for information and synthesize an accurate answer with citations."},
            {"role": "user", "content": "What is the current population of Tokyo, and how has it changed over the last decade?"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo current population 2024\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "Tokyo metropolitan area population (2024): approximately 13.96 million in the 23 special wards, 37.4 million in the Greater Tokyo Area. The population of the 23 wards peaked in 2020 at 14.04 million before a slight decline attributed to COVID-19 migration patterns. Source: Tokyo Metropolitan Government Statistics Bureau."},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_2", "type": "function", "function": {"name": "web_search", "arguments": "{\"query\": \"Tokyo population change 2014 to 2024 decade trend\"}"}}]},
            {"role": "tool", "tool_call_id": "call_2", "content": "Tokyo population trend 2014-2024: Growth from 13.35M (2014) to peak of 14.04M (2020), followed by decline to 13.96M (2024) due to COVID-19 remote work migration. Net change: +4.6% over decade but trajectory reversed post-2020. Source: Tokyo Metropolitan Government Statistics Bureau (2024)."},
        ],
        "tools": [
            {"type": "function", "function": {"name": "web_search", "description": "Search the web for information", "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}}
        ]
    });
    let resp5_fixed = serde_json::json!({
        "id": "chatcmpl-004",
        "model": "gpt-4o-2024-08-06",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "# Tokyo Population Analysis\n\n## Current Population\nTokyo's 23 special wards currently have a population of approximately **13.96 million** (2024), with the Greater Tokyo Area home to 37.4 million people.\n\n## Decade Trend (2014-2024)\nTokyo's population followed a **growth-then-decline** pattern over the past decade:\n- 2014: 13.35 million\n- 2020: 14.04 million (peak)\n- 2024: 13.96 million (post-COVID decline)\n\nThis represents a net **+4.6% increase** over the full decade, but the trajectory **reversed after 2020** due to COVID-19 driving remote work adoption and outmigration to surrounding prefectures.\n\n## Key Insight\nThe commonly cited projection of continued growth to 14.2M has not materialized. Tokyo's population actually **declined by ~80,000** from its 2020 peak, marking the first sustained decline in decades.\n\n## Sources\n- Tokyo Metropolitan Government Statistics Bureau (2024)\n- National Institute of Population and Social Security Research"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 520, "completion_tokens": 195, "total_tokens": 715}
    });

    create_step_with_blobs(store, &fork.id, &session.id, 5, StepType::LlmCall, StepStatus::Success,
        "gpt-4o", 1320, 520, 195, &req5_fixed, &resp5_fixed, None)?;

    println!("  {} Created main timeline (5 steps, fails at step 5 — hallucination)", "├".dimmed());
    println!("  {} Created fork at step 4 with corrected tool response", "├".dimmed());
    println!("  {} Fork step 5 produces accurate answer", "└".dimmed());

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_step_with_blobs(
    store: &Store,
    timeline_id: &str,
    session_id: &str,
    step_number: u32,
    step_type: rewind_store::StepType,
    status: rewind_store::StepStatus,
    model: &str,
    duration_ms: u64,
    tokens_in: u64,
    tokens_out: u64,
    request: &serde_json::Value,
    response: &serde_json::Value,
    error: Option<&str>,
) -> Result<()> {
    let req_hash = store.blobs.put_json(request)?;
    let resp_hash = store.blobs.put_json(response)?;

    let step = rewind_store::Step {
        id: uuid::Uuid::new_v4().to_string(),
        timeline_id: timeline_id.to_string(),
        session_id: session_id.to_string(),
        step_number,
        step_type,
        status,
        created_at: chrono::Utc::now(),
        duration_ms,
        tokens_in,
        tokens_out,
        model: model.to_string(),
        request_blob: req_hash,
        response_blob: resp_hash,
        error: error.map(String::from),
        span_id: None,
        tool_name: None,
    };

    store.create_step(&step)?;
    Ok(())
}

// ── Fix Command ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn cmd_fix(
    session_ref: String,
    diagnosis_model: String,
    apply: bool,
    agent_command: Option<String>,
    upstream: String,
    port: u16,
    step_override: Option<u32>,
    expected: Option<String>,
    hypothesis: Option<String>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let steps = store.get_steps(&timeline.id)?;
    if steps.is_empty() {
        bail!("Session has no steps to diagnose.");
    }

    if matches!(session.source, rewind_store::SessionSource::Direct) && apply {
        bail!(
            "This session was recorded in direct mode. \
             --apply requires a proxy-recorded session. \
             Re-record with `rewind record` to use --apply."
        );
    }

    let failure_step = find_failure_step(&steps, &session, step_override);
    let failure_step_num = failure_step.step_number;

    // ── Hypothesis mode: skip diagnosis, parse fix directly ──
    let result = if let Some(ref hyp) = hypothesis {
        if !apply {
            bail!("--hypothesis requires --apply to test the fix.");
        }
        parse_hypothesis(hyp, failure_step_num)?
    } else {
        // ── Diagnosis mode: call LLM ──
        if !json_output {
            eprintln!(
                "{}\n",
                format!(
                    "⏪ Diagnosing session \"{}\" ({} steps)...",
                    session.name, steps.len()
                )
                .cyan()
                .bold()
            );
        }

        let payload = build_diagnosis_payload(
            &store, &session, &steps, failure_step, failure_step_num,
            &expected, &diagnosis_model,
        );
        run_fix_subprocess(&payload)?
    };

    let fix_type = result.get("fix_type").and_then(|v| v.as_str()).unwrap_or("no_fix");

    // ── Diagnosis-only mode ──
    if !apply {
        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            print_fix_diagnosis(&result, failure_step);
        }
        return Ok(());
    }

    // ── Apply mode ──
    if fix_type == "no_fix" {
        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            print_fix_diagnosis(&result, failure_step);
            eprintln!("  {} No proxy-level fix available. The issue is in agent code.", "⚠".yellow().bold());
        }
        return Ok(());
    }

    let fork_from = result.get("fork_from").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let fork_from = fork_from.max(1).min(steps.len() as u32);

    let rewrite_config = build_rewrite_config(&result);

    // Dry-run preview
    if !yes && !json_output {
        print_fix_diagnosis(&result, failure_step);
        println!();
        println!("  {}", "⏪ Proposed fix:".cyan().bold());
        println!("  {} {}", "Fix type:".dimmed(), fix_type.yellow().bold());
        if let Some(ref m) = rewrite_config.model {
            println!("  {} {} → {}", "Model:".dimmed(), failure_step.model, m.green());
        }
        if rewrite_config.system_inject.is_some() {
            println!("  {} system message will be injected", "Inject:".dimmed());
        }
        if let Some(t) = rewrite_config.temperature {
            println!("  {} {}", "Temperature:".dimmed(), t);
        }
        println!("  {} step {}", "Fork from:".dimmed(), fork_from);
        println!("  {} {} (0 tokens, 0ms)", "Steps cached:".dimmed(), fork_from);
        println!("  {} {}+ (forwarded with rewrite)", "Steps live:".dimmed(), fork_from + 1);
        println!();

        eprint!("  Apply this fix? [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("  Cancelled.");
            return Ok(());
        }
    }

    // Fork
    let engine = ReplayEngine::new(&store);
    let parent_steps = engine.get_full_timeline_steps(&timeline.id, &session.id)?;
    let fork = engine.fork(&session.id, &timeline.id, fork_from, &format!("fix-{}", fix_type))?;

    // Start proxy with rewrites (or without for retry_step)
    let proxy = if fix_type == "retry_step" {
        ProxyServer::new_fork_execute(
            store, &session.id, &fork.id, parent_steps.clone(), fork_from, &upstream,
        )?
    } else {
        ProxyServer::new_fork_execute_with_rewrites(
            store, &session.id, &fork.id, parent_steps.clone(), fork_from, &upstream,
            rewrite_config,
        )?
    };

    if !json_output {
        println!();
        println!("{}", "⏪ Fix applied — proxy running".cyan().bold());
        println!("  {} {}", "Session:".dimmed(), session.name.white().bold());
        println!("  {} step {}", "Fork at:".dimmed(), fork_from);
        println!("  {} {}", "Fix type:".dimmed(), fix_type.yellow());
        println!("  {} {}", "Fork ID:".dimmed(), fork.id[..12].to_string().dimmed());
        println!("  {} {}", "Proxy:".dimmed(), format!("http://127.0.0.1:{}", port).yellow());
        println!();
    }

    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", port).parse()?;

    if let Some(ref cmd_str) = agent_command {
        // ── --command mode: run agent automatically ──
        if !json_output {
            eprintln!("  {} {}", "Running:".dimmed(), cmd_str.green());
            eprintln!();
        }

        let proxy_handle = tokio::spawn(async move {
            let _ = proxy.run(addr).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let status = run_agent_command(cmd_str, port)?;
        if !json_output {
            if status.success() {
                eprintln!("  {} Agent finished (exit 0)", "✓".green());
            } else {
                eprintln!("  {} Agent finished (exit {})", "⚠".yellow(), status);
            }
        }

        proxy_handle.abort();
    } else {
        // ── --apply mode: wait for manual re-run ──
        if !json_output {
            println!("  {} Point your agent at this proxy:", "→".cyan());
            println!("    {}", format!("export OPENAI_BASE_URL=http://127.0.0.1:{}/v1", port).green());
            println!();
            println!("  {} to stop and score.", "Ctrl+C".yellow().bold());
            println!();
        }

        proxy.run(addr).await?;
    }

    // ── Score both timelines ──
    if !json_output {
        eprintln!();
        eprintln!("{}", "⏪ Scoring both timelines...".cyan().bold());
    }

    let score_store = Store::open_default()?;
    let all_timelines = score_store.get_timelines(&session.id)?;
    let forks: Vec<_> = all_timelines.iter().filter(|t| t.id == fork.id).collect();
    print_session_savings(&score_store, &forks);

    if !json_output {
        println!();
        println!("  {} Compare in detail:", "→".cyan());
        println!("    {}", format!("rewind diff {} {} {}", &session.id[..8], &timeline.id[..8], &fork.id[..8]).green());
        println!("    {}", format!("rewind eval score {} --compare-timelines -e task_completion", &session.id[..8]).green());
    }

    println!();
    Ok(())
}

fn build_diagnosis_payload(
    store: &Store,
    session: &rewind_store::Session,
    steps: &[rewind_store::Step],
    failure_step: &rewind_store::Step,
    failure_step_num: u32,
    expected: &Option<String>,
    diagnosis_model: &str,
) -> serde_json::Value {
    let failure_request = if !failure_step.request_blob.is_empty() {
        store.blobs.get_json::<serde_json::Value>(&failure_step.request_blob).ok()
    } else {
        None
    };
    let failure_response = if !failure_step.response_blob.is_empty() {
        store.blobs.get_json::<serde_json::Value>(&failure_step.response_blob).ok()
    } else {
        None
    };

    let preceding_steps: Vec<serde_json::Value> = steps.iter()
        .filter(|s| {
            s.step_number < failure_step_num
                && s.step_number >= failure_step_num.saturating_sub(3)
        })
        .map(|s| {
            let req = if !s.request_blob.is_empty() {
                store.blobs.get_json::<serde_json::Value>(&s.request_blob).ok()
            } else {
                None
            };
            let resp = if !s.response_blob.is_empty() {
                store.blobs.get_json::<serde_json::Value>(&s.response_blob).ok()
            } else {
                None
            };
            serde_json::json!({
                "step_number": s.step_number,
                "request": req,
                "response": resp,
            })
        })
        .collect();

    let steps_summary: Vec<serde_json::Value> = steps.iter().map(|s| {
        serde_json::json!({
            "step_number": s.step_number,
            "step_type": format!("{:?}", s.step_type).to_lowercase(),
            "status": format!("{:?}", s.status).to_lowercase(),
            "model": s.model,
            "tokens_in": s.tokens_in,
            "tokens_out": s.tokens_out,
            "duration_ms": s.duration_ms,
            "tool_name": s.tool_name,
            "error": s.error,
        })
    }).collect();

    serde_json::json!({
        "session": {
            "id": session.id,
            "name": session.name,
            "status": format!("{:?}", session.status).to_lowercase(),
            "total_steps": session.total_steps,
        },
        "steps": steps_summary,
        "failure_step": failure_step_num,
        "failure_context": {
            "request": failure_request,
            "response": failure_response,
            "preceding_steps": preceding_steps,
        },
        "expected": expected,
        "config": {
            "model": diagnosis_model,
        },
    })
}

fn parse_hypothesis(hyp: &str, failure_step_num: u32) -> Result<serde_json::Value> {
    let parts: Vec<&str> = hyp.splitn(2, ':').collect();
    let fix_type = parts[0];
    let param = parts.get(1).unwrap_or(&"");

    let valid_types = ["swap_model", "inject_system", "adjust_temperature", "retry_step"];
    if !valid_types.contains(&fix_type) {
        bail!(
            "Unknown fix type '{}'. Valid: {}",
            fix_type,
            valid_types.join(", ")
        );
    }

    let fix_params = match fix_type {
        "swap_model" => {
            if param.is_empty() {
                bail!("swap_model requires a model name, e.g., --hypothesis swap_model:gpt-4o");
            }
            serde_json::json!({"model": param})
        }
        "inject_system" => {
            if param.is_empty() {
                bail!("inject_system requires content, e.g., --hypothesis 'inject_system:Be more careful'");
            }
            serde_json::json!({"content": param})
        }
        "adjust_temperature" => {
            let temp: f64 = param.parse().map_err(|_| anyhow::anyhow!(
                "adjust_temperature requires a number, e.g., --hypothesis adjust_temperature:0.2"
            ))?;
            serde_json::json!({"temperature": temp})
        }
        "retry_step" => serde_json::json!({}),
        _ => serde_json::json!({}),
    };

    Ok(serde_json::json!({
        "root_cause": format!("User hypothesis: {}", hyp),
        "failed_step": failure_step_num,
        "fork_from": failure_step_num.saturating_sub(1).max(1),
        "fix_type": fix_type,
        "fix_params": fix_params,
        "explanation": format!("Testing user-specified fix: {}", hyp),
        "confidence": "manual",
    }))
}

fn build_rewrite_config(result: &serde_json::Value) -> rewind_proxy::RewriteConfig {
    let fix_type = result.get("fix_type").and_then(|v| v.as_str()).unwrap_or("no_fix");
    let fix_params = result.get("fix_params").cloned().unwrap_or(serde_json::json!({}));

    rewind_proxy::RewriteConfig {
        model: if fix_type == "swap_model" {
            fix_params.get("model").and_then(|v| v.as_str()).map(String::from)
        } else {
            None
        },
        system_inject: if fix_type == "inject_system" {
            fix_params.get("content").and_then(|v| v.as_str()).map(String::from)
        } else {
            None
        },
        temperature: if fix_type == "adjust_temperature" {
            fix_params.get("temperature").and_then(|v| v.as_f64())
        } else {
            None
        },
    }
}

fn run_agent_command(cmd_str: &str, port: u16) -> Result<std::process::ExitStatus> {
    use std::process::Command;

    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        bail!("Empty --command string");
    }

    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    cmd.env("OPENAI_BASE_URL", format!("http://127.0.0.1:{}/v1", port));
    cmd.env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}/anthropic", port));

    let status = cmd.status().map_err(|e| {
        anyhow::anyhow!("Failed to run agent command '{}': {}", cmd_str, e)
    })?;
    Ok(status)
}

fn find_failure_step<'a>(
    steps: &'a [rewind_store::Step],
    session: &rewind_store::Session,
    step_override: Option<u32>,
) -> &'a rewind_store::Step {
    if let Some(n) = step_override {
        if let Some(s) = steps.iter().find(|s| s.step_number == n) {
            return s;
        }
    }

    if let Some(s) = steps.iter().find(|s| s.status == rewind_store::StepStatus::Error) {
        return s;
    }

    if matches!(session.status, rewind_store::SessionStatus::Failed) {
        return steps.last().unwrap();
    }

    eprintln!(
        "  {} No error step found. Analyzing last step. For better results, pass {}.",
        "⚠".yellow().bold(),
        "--expected 'description of correct behavior'".dimmed()
    );
    steps.last().unwrap()
}

fn run_fix_subprocess(payload: &serde_json::Value) -> Result<serde_json::Value> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let payload_str = serde_json::to_string(payload)?;

    let mut cmd = Command::new("python3");
    cmd.args(["-m", "rewind_agent.fix"]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!(
            "Failed to spawn diagnosis subprocess (python3 -m rewind_agent.fix): {}. \
             Is rewind-agent installed? Run: pip install rewind-agent[openai]",
            e
        ))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(payload_str.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Failed to write diagnostic payload ({} bytes) to subprocess: {}", payload_str.len(), e);
        }
    }

    let timeout = std::time::Duration::from_secs(120);
    loop {
        match child.try_wait()? {
            Some(status) => {
                let mut stdout_str = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut stdout_str);
                }
                stdout_str.truncate(100_000);

                if !status.success() {
                    if let Ok(result) = serde_json::from_str::<serde_json::Value>(stdout_str.trim()) {
                        return Ok(result);
                    }
                    let mut stderr_str = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = stderr.read_to_string(&mut stderr_str);
                    }
                    stderr_str.truncate(1000);
                    bail!(
                        "Diagnosis subprocess failed (exit {}): {}",
                        status, stderr_str.trim()
                    );
                }

                let result: serde_json::Value = serde_json::from_str(stdout_str.trim())
                    .map_err(|e| anyhow::anyhow!(
                        "Diagnosis output is not valid JSON: {}. Got: '{}'",
                        e, stdout_str.chars().take(200).collect::<String>()
                    ))?;
                return Ok(result);
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("Diagnosis timed out after {}s", timeout.as_secs());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn print_fix_diagnosis(result: &serde_json::Value, failure_step: &rewind_store::Step) {
    let fix_type = result.get("fix_type").and_then(|v| v.as_str()).unwrap_or("no_fix");
    let confidence = result.get("confidence").and_then(|v| v.as_str()).unwrap_or("low");
    let root_cause = result.get("root_cause").and_then(|v| v.as_str()).unwrap_or("Unknown");
    let explanation = result.get("explanation").and_then(|v| v.as_str()).unwrap_or("");
    let fork_from = result.get("fork_from").and_then(|v| v.as_u64()).unwrap_or(0);

    let step_type = format!("{:?}", failure_step.step_type).to_lowercase();
    let status = format!("{:?}", failure_step.status).to_lowercase();

    println!("  {} Step {} — {} ({}) — {}", "Failure:".dimmed(),
        failure_step.step_number, step_type, failure_step.model, status);

    if let Some(ref err) = failure_step.error {
        println!("  {} {}", "Error:".dimmed(), err.red());
    }

    println!("  {} {}", "Root cause:".dimmed(), root_cause.white());
    println!();

    if fix_type != "no_fix" {
        let fix_params = result.get("fix_params").cloned().unwrap_or(serde_json::json!({}));
        let params_str = if fix_params.is_object() && !fix_params.as_object().unwrap().is_empty() {
            serde_json::to_string(&fix_params).unwrap_or_default()
        } else {
            String::new()
        };

        println!("  {} {} {}", "Suggested fix:".dimmed(),
            fix_type.yellow().bold(),
            if params_str.is_empty() { String::new() } else { format!("→ {}", params_str) });
        if !explanation.is_empty() {
            println!("  {} {}", "Reasoning:".dimmed(), explanation);
        }
        println!("  {} {}", "Confidence:".dimmed(), match confidence {
            "high" => confidence.green().bold().to_string(),
            "medium" => confidence.yellow().to_string(),
            _ => confidence.red().to_string(),
        });

        if fork_from > 0 {
            println!();
            println!("  To apply this fix automatically:");
            println!("    {}", format!("rewind fix {} --apply", "latest").green());
        }
    } else {
        println!("  {} {}", "Diagnosis:".dimmed(), "no_fix — issue is in agent code, not LLM behavior".yellow());
        if !explanation.is_empty() {
            println!("  {} {}", "Reasoning:".dimmed(), explanation);
        }
        println!("  {} {}", "Confidence:".dimmed(), confidence.dimmed());
    }

    println!();
}

// ── Helpers ───────────────────────────────────────────────────

fn resolve_session(store: &Store, session_ref: &str) -> Result<rewind_store::Session> {
    if session_ref == "latest" {
        store.get_latest_session()?.context("No sessions found. Run 'rewind demo' to create demo data.")
    } else {
        // Try exact match first, then prefix match
        if let Some(session) = store.get_session(session_ref)? {
            return Ok(session);
        }
        // Prefix match
        let sessions = store.list_sessions()?;
        sessions.into_iter()
            .find(|s| s.id.starts_with(session_ref))
            .context(format!("Session not found: {}", session_ref))
    }
}

fn resolve_timeline_ref(timelines: &[rewind_store::Timeline], reference: &str) -> Result<String> {
    // Try exact match
    if let Some(t) = timelines.iter().find(|t| t.id == reference) {
        return Ok(t.id.clone());
    }
    // Try prefix match
    if let Some(t) = timelines.iter().find(|t| t.id.starts_with(reference)) {
        return Ok(t.id.clone());
    }
    // Try label match
    if let Some(t) = timelines.iter().find(|t| t.label == reference) {
        return Ok(t.id.clone());
    }
    bail!("Timeline not found: {}", reference)
}

fn extract_response_preview(val: &serde_json::Value) -> String {
    // OpenAI
    if let Some(content) = val.pointer("/choices/0/message/content").and_then(|c| c.as_str()) {
        return content.replace('\n', " ").chars().take(100).collect();
    }
    // Tool calls
    if let Some(calls) = val.pointer("/choices/0/message/tool_calls").and_then(|c| c.as_array()) {
        let names: Vec<&str> = calls.iter()
            .filter_map(|c| c.pointer("/function/name").and_then(|n| n.as_str()))
            .collect();
        return format!("tool_calls: {}", names.join(", "));
    }
    // Anthropic
    if let Some(content) = val.get("content").and_then(|c| c.as_array())
        && let Some(text) = content.first().and_then(|b| b.get("text")).and_then(|t| t.as_str()) {
            return text.replace('\n', " ").chars().take(100).collect();
        }
    String::new()
}

fn format_time_ago(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now - dt;

    if duration.num_seconds() < 60 {
        "just now".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else {
        format!("{}d ago", duration.num_days())
    }
}

// ── Export Commands ─────────────────────────────────────────

async fn cmd_export_otel(args: OtelExportArgs) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &args.session)?;

    // Parse protocol
    let proto = match args.protocol.to_lowercase().as_str() {
        "grpc" => rewind_otel::export::Protocol::Grpc,
        "http" | "http/protobuf" => rewind_otel::export::Protocol::Http,
        other => bail!("Unknown protocol '{}'. Use 'http' or 'grpc'.", other),
    };

    // Parse headers (KEY=VALUE format)
    let parsed_headers: Vec<(String, String)> = args.headers
        .iter()
        .map(|h| {
            let (k, v) = h.split_once('=').with_context(|| format!("Invalid header format '{}'. Use KEY=VALUE.", h))?;
            Ok((k.to_string(), v.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;

    // Extract options
    let extract_opts = rewind_otel::extract::ExtractOptions {
        timeline_id: args.timeline,
        all_timelines: args.all_timelines,
    };

    // Step 1: Extract data synchronously from Store (Store is not Send)
    // Use stderr for status messages so --dry-run stdout is clean JSON.
    eprintln!(
        "{} Extracting session {} ...",
        "⏪".bold(),
        session.id[..8].yellow()
    );

    let data = rewind_otel::extract::extract_session_data(&store, &session.id, &extract_opts)?;

    let total = data.total_steps();
    let tl_count = data.timelines.len();
    eprintln!(
        "   {} steps across {} timeline{}",
        total.to_string().cyan(),
        tl_count,
        if tl_count == 1 { "" } else { "s" }
    );

    // Step 2: Export asynchronously (no Store needed)
    let config = rewind_otel::export::ExportConfig {
        endpoint: args.endpoint.clone(),
        protocol: proto,
        headers: parsed_headers,
        include_content: args.include_content,
    };

    let span_count = if args.dry_run {
        // Build the proper ExportTraceServiceRequest and output as JSON.
        // This is importable via `rewind import otel --json-file`.
        let request = rewind_otel::export::build_otlp_request(&data, &config);
        let json = serde_json::to_string_pretty(&request)?;
        println!("{json}");
        request.resource_spans.iter()
            .flat_map(|rs| &rs.scope_spans)
            .map(|ss| ss.spans.len())
            .sum::<usize>()
    } else {
        println!(
            "{} Exporting to {} ...",
            "📡".bold(),
            args.endpoint.cyan()
        );
        rewind_otel::export::export_to_otlp(&data, &config)?
    };

    eprintln!(
        "\n{} Exported {} OTel spans (trace_id: {})",
        "✓".green().bold(),
        span_count.to_string().cyan(),
        rewind_otel::export::trace_id_from_session(&session.id).to_string()[..16].yellow()
    );

    if args.include_content {
        eprintln!(
            "   {} Full message content included (--include-content)",
            "⚠".yellow()
        );
    }

    Ok(())
}

// ── Share Command ──────────────────────────────────────────

fn cmd_share(session_ref: String, include_content: bool, output: Option<std::path::PathBuf>, yes: bool) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;

    // Content warning + confirmation
    if include_content && !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("--include-content requires interactive confirmation. Use --yes to skip.");
        }
        eprintln!(
            "{} This file will contain full LLM request/response content.",
            "⚠".yellow().bold(),
        );
        eprint!("  Proceed? [y/N] ");
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    println!(
        "{} Exporting session {}...",
        "⏪".bold(),
        session.name.cyan(),
    );

    let exported = rewind_store::export::serialize_session(&store, &session.id, include_content)?;
    let html = share::generate_share_html(&exported)?;

    let out_path = output.unwrap_or_else(|| {
        let short_id = if session.id.len() >= 8 { &session.id[..8] } else { &session.id };
        std::path::PathBuf::from(format!("rewind-session-{}.html", short_id))
    });

    std::fs::write(&out_path, &html)?;

    let size_kb = html.len() / 1024;
    println!();
    println!(
        "{} Shared session saved to: {}",
        "✓".green().bold(),
        out_path.display().to_string().cyan(),
    );
    println!(
        "   {} Open in any browser. Share via Slack, email, or any file-sharing tool.",
        "→".bold(),
    );
    println!(
        "   {} Contains: {} ({}KB)",
        "→".bold(),
        if include_content { "metadata + full content" } else { "metadata only (no LLM content)" },
        size_kb,
    );

    Ok(())
}

// ── Import Commands ─────────────────────────────────────────

fn cmd_import_otel(args: OtelImportArgs) -> Result<()> {
    let store = Store::open_default()?;

    let (bytes, is_json) = if let Some(ref path) = args.file {
        let data = std::fs::read(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        println!(
            "{} Reading protobuf file: {} ({} bytes)",
            "⏪".bold(),
            path.display().to_string().cyan(),
            data.len()
        );
        (data, false)
    } else if let Some(ref path) = args.json_file {
        let data = std::fs::read(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        println!(
            "{} Reading JSON file: {} ({} bytes)",
            "⏪".bold(),
            path.display().to_string().cyan(),
            data.len()
        );
        (data, true)
    } else {
        bail!("Specify --file <path> or --json-file <path>");
    };

    let request = if is_json {
        rewind_otel::ingest::decode_otlp_json_request(&bytes)?
    } else {
        rewind_otel::ingest::decode_otlp_request(&bytes, false)?
    };

    let opts = rewind_otel::ingest::IngestOptions {
        session_name: args.name,
    };

    let result = rewind_otel::ingest::ingest_trace_request(request, &store, &opts)?;

    println!(
        "\n{} Imported {} spans → {} steps (session: {})",
        "✓".green().bold(),
        result.spans_ingested.to_string().cyan(),
        result.steps_created.to_string().cyan(),
        result.session_id[..8].yellow()
    );

    if result.replay_possible {
        println!(
            "   {} Content blobs stored — session is replayable",
            "🔁".bold()
        );
    } else {
        println!(
            "   {} No content blobs — session is inspect-only (not replayable)",
            "👁".bold()
        );
    }

    println!(
        "   {} View with: rewind show {}",
        "→".bold(),
        result.session_id[..8].cyan()
    );

    Ok(())
}

fn cmd_import_from_langfuse(args: LangfuseImportArgs) -> Result<()> {
    println!(
        "{} Importing trace {} from {}",
        "⏪".bold(),
        args.trace.cyan(),
        args.host.dimmed()
    );

    let payload = serde_json::json!({
        "trace_id": args.trace,
        "public_key": args.public_key,
        "secret_key": args.secret_key,
        "host": args.host,
        "session_name": args.name,
        "rewind_endpoint": "http://127.0.0.1:4800",
    });
    let payload_str = serde_json::to_string(&payload)?;

    let mut cmd = std::process::Command::new("python3");
    cmd.args(["-m", "rewind_agent.langfuse_import"]);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "Failed to run python3 -m rewind_agent.langfuse_import: {}. \
             Is rewind-agent installed? (pip install rewind-agent)",
            e
        )
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(payload_str.as_bytes());
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let error_msg = if !stdout.is_empty() {
            if let Ok(result) = serde_json::from_str::<serde_json::Value>(&stdout) {
                result.get("error").and_then(|e| e.as_str()).unwrap_or(&stderr).to_string()
            } else {
                stderr.to_string()
            }
        } else {
            stderr.to_string()
        };
        bail!("Langfuse import failed: {}", error_msg.trim());
    }

    let result: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("Failed to parse import result: {}", e))?;

    let session_id = result.get("session_id").and_then(|v| v.as_str()).unwrap_or("<unknown>");

    println!(
        "\n{} Imported Langfuse trace → Rewind session {}",
        "✓".green().bold(),
        if session_id.len() >= 8 { &session_id[..8] } else { session_id }.yellow()
    );
    println!(
        "   {} View with: rewind show {}",
        "→".bold(),
        if session_id.len() >= 8 { &session_id[..8] } else { session_id }.cyan()
    );

    Ok(())
}

