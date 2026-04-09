use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use rewind_proxy::ProxyServer;
use rewind_replay::ReplayEngine;
use rewind_store::Store;
use rewind_tui::TuiApp;
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

    /// List snapshots
    Snapshots,

    /// Show Instant Replay cache statistics
    Cache,

    /// Seed demo data to showcase the tool
    Demo,
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
        Commands::Record { name, port, upstream, replay } => cmd_record(name, port, upstream, replay).await,
        Commands::Sessions => cmd_sessions(),
        Commands::Inspect { session } => cmd_inspect(session),
        Commands::Show { session } => cmd_show(session),
        Commands::Fork { session, at, label } => cmd_fork(session, at, label),
        Commands::Diff { session, left, right } => cmd_diff(session, left, right),
        Commands::Snapshot { directory, label } => cmd_snapshot(directory, label),
        Commands::Restore { snapshot } => cmd_restore(snapshot),
        Commands::Snapshots => cmd_snapshots(),
        Commands::Cache => cmd_cache(),
        Commands::Demo => cmd_demo(),
    }
}

async fn cmd_record(name: String, port: u16, upstream: String, replay: bool) -> Result<()> {
    let store = Store::open_default()?;
    let proxy = ProxyServer::new(store, &name, &upstream, replay)?;

    println!("{}", "⏪ Rewind — Recording Started".cyan().bold());
    println!();
    println!("  {} {}", "Session:".dimmed(), name.white().bold());
    println!("  {} {}", "Proxy:".dimmed(), format!("http://127.0.0.1:{}", port).yellow());
    println!("  {} {}", "Upstream:".dimmed(), upstream.dimmed());
    if replay {
        println!("  {} {}", "Replay:".dimmed(), "ON — identical requests served from cache at $0".green().bold());
    }
    println!();
    println!("  {} Set your agent's base URL to intercept calls:", "→".cyan());
    println!("    {}", format!("export OPENAI_BASE_URL=http://127.0.0.1:{}/v1", port).green());
    println!();
    println!("  {} to stop recording.", "Ctrl+C".yellow().bold());
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
        "  {} {:^12} {:^20} {:>6} {:>10} {:>12} {}",
        "STATUS".dimmed(),
        "ID".dimmed(),
        "NAME".dimmed(),
        "STEPS".dimmed(),
        "TOKENS".dimmed(),
        "COST".dimmed(),
        "CREATED".dimmed(),
    );
    println!("  {}", "─".repeat(85).dimmed());

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
            "  {}  {:>12} {:>20} {:>6} {:>10} {:>12} {}",
            status_icon,
            short_id.dimmed(),
            session.name.white().bold(),
            session.total_steps.to_string().yellow(),
            session.total_tokens.to_string().blue(),
            format!("${:.4}", session.total_cost_usd).green(),
            ago.dimmed(),
        );
    }

    println!();
    println!("  Run {} to inspect a session.", "rewind inspect <session-id>".green());
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

fn cmd_show(session_ref: String) -> Result<()> {
    let store = Store::open_default()?;
    let session = resolve_session(&store, &session_ref)?;
    let timeline = store.get_root_timeline(&session.id)?
        .context("No timeline found for session")?;

    let engine = ReplayEngine::new(&store);
    let steps = engine.get_full_timeline_steps(&timeline.id, &session.id)?;

    println!("{}", "⏪ Rewind — Session Trace".cyan().bold());
    println!();
    println!("  {} {}", "Session:".dimmed(), session.name.white().bold());
    println!("  {} {}", "ID:".dimmed(), session.id.dimmed());
    println!("  {} {}", "Steps:".dimmed(), steps.len().to_string().yellow());
    println!("  {} {}", "Cost:".dimmed(), format!("${:.6}", session.total_cost_usd).green());
    println!();

    for step in &steps {
        let status_icon = match step.status {
            rewind_store::StepStatus::Success => "✓".green(),
            rewind_store::StepStatus::Error => "✗".red(),
            rewind_store::StepStatus::Pending => "…".yellow(),
        };

        let connector = if step.step_number == 1 { "┌" } else if step.step_number == steps.len() as u32 { "└" } else { "├" };

        println!(
            "  {} {} {} {:>8} {:>8} {:>10} {:>12} {}",
            connector.dimmed(),
            status_icon,
            step.step_type.icon(),
            format!("Step {}", step.step_number).white().bold(),
            step.model.magenta(),
            format!("{}ms", step.duration_ms).yellow(),
            format!("${:.6}", step.cost_usd).green(),
            format!("{}↓ {}↑", step.tokens_in, step.tokens_out).blue(),
        );

        if let Some(ref err) = step.error {
            println!("  │   {} {}", "ERROR:".red().bold(), err.red());
        }

        // Show response preview
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

    println!();
    println!("  Run {} to explore interactively.", format!("rewind inspect {}", &session.id[..8]).green());
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
                    let left_cost = sd.left.as_ref().map(|s| format!("${:.4}", s.cost_usd)).unwrap_or_default();
                    let right_cost = sd.right.as_ref().map(|s| format!("${:.4}", s.cost_usd)).unwrap_or_default();
                    format!("{} {} {} {} {}",
                        format!("[{}]", left_status).red(),
                        left_cost.dimmed(),
                        "→".dimmed(),
                        format!("[{}]", right_status).green(),
                        right_cost.dimmed(),
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

    println!("  {} Demo session created!", "✓".green().bold());
    println!();
    println!("  Try these commands:");
    println!("    {} — list all sessions", "rewind sessions".green());
    println!("    {} — see the trace", "rewind show latest".green());
    println!("    {} — interactive TUI", "rewind inspect latest".green());
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

    let target_dir = std::path::Path::new(&snapshot.directory);
    archive.unpack(target_dir)?;

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
        println!("  {} {}", "Total saved:".dimmed(), format!("${:.4}", stats.total_saved_usd).green().bold());
    }
    println!();
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
        "gpt-4o", 320, 156, 28, 0.00062, &req1, &resp1, None)?;

    // Step 2: Tool result — web search
    let req2 = serde_json::json!({
        "role": "tool",
        "tool_call_id": "call_1",
        "content": "Tokyo metropolitan area population (2024): approximately 13.96 million in the 23 special wards, 37.4 million in the Greater Tokyo Area. The population of the 23 wards peaked in 2020 at 14.04 million before a slight decline attributed to COVID-19 migration patterns. Source: Tokyo Metropolitan Government Statistics Bureau."
    });
    let resp2 = serde_json::json!({"status": "delivered"});

    create_step_with_blobs(store, &timeline.id, &session.id, 2, StepType::ToolResult, StepStatus::Success,
        "tool", 45, 0, 0, 0.0, &req2, &resp2, None)?;

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
        "gpt-4o", 890, 312, 35, 0.00113, &req3, &resp3, None)?;

    // Step 4: Tool result — search about decade trend (THIS HAS MISLEADING DATA)
    let req4 = serde_json::json!({
        "role": "tool",
        "tool_call_id": "call_2",
        "content": "ERROR: Search API rate limited. Cached result returned from 2019 dataset. Tokyo population trend 2014-2019: steady growth from 13.35M to 13.96M in 23 wards (+4.6%). National Institute of Population projections (2019): expected continued growth through 2025, reaching 14.2M. Note: this data predates COVID-19 impacts."
    });
    let resp4 = serde_json::json!({"status": "delivered"});

    create_step_with_blobs(store, &timeline.id, &session.id, 4, StepType::ToolResult, StepStatus::Success,
        "tool", 38, 0, 0, 0.0, &req4, &resp4, None)?;

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
        "gpt-4o", 1450, 520, 180, 0.00310, &req5, &resp5,
        Some("HALLUCINATION: Agent used stale 2019 projection (14.2M) as current fact, ignored COVID-19 dip to 13.96M, and claimed 'no significant disruptions' despite search result explicitly noting COVID impacts."))?;

    // Update session totals
    store.update_session_stats(
        &session.id,
        5,
        0.00062 + 0.00113 + 0.00310,
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
        "gpt-4o", 1320, 520, 195, 0.00323, &req5_fixed, &resp5_fixed, None)?;

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
    cost_usd: f64,
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
        cost_usd,
        model: model.to_string(),
        request_blob: req_hash,
        response_blob: resp_hash,
        error: error.map(String::from),
    };

    store.create_step(&step)?;
    Ok(())
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

