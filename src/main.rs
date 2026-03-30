//! Forgejo Runner Orchestrator for Hetzner Cloud.
//!
//! Automatically creates Hetzner servers as Forgejo Actions runners when tasks
//! are waiting and deletes them cost-optimized after completion.
//!
//! # How it works
//!
//! 1. Polls the Forgejo API for active tasks (waiting/running)
//! 2. On active task: Create Hetzner server (if not present)
//! 3. On no active tasks: Delete server (after minimum runtime)
//! 4. Deletion ideally 5min before next billing cycle
//!
//! # Configuration
//!
//! Expects `config/config.toml` with Forgejo and Hetzner credentials.
//! Expects `config/runner.toml` with Forgejo Runner configuration.

mod cloud_init;
mod config;
mod csv_log;
mod forgejo;
mod hetzner;
mod state;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::sleep;
use tracing::{error, info, warn};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, Layer};

use crate::cloud_init::generate_cloud_init;
use crate::config::{load_runner_config, Config};
use crate::csv_log::CsvLogger;
use crate::forgejo::ForgejoClient;
use crate::hetzner::HetznerClient;
use crate::state::{OrchestratorState, RunnerState};

/// Buffer in minutes before billing cycle for optimal deletion.
const BILLING_BUFFER_MINUTES: u64 = 5;

/// Default path for configuration.
const CONFIG_PATH: &str = "config/config.toml";

/// Default path for the pre-registered Forgejo runner credentials.
const RUNNER_CONFIG_PATH: &str = "config/data/.runner";

/// Default path for persisted state.
const STATE_PATH: &str = "config/state.json";

/// Default path for logs.
const LOG_DIR: &str = "logs";

/// Default path for example configuration.
const CONFIG_EXAMPLE_PATH: &str = "config/config.example.toml";

/// Content of the example configuration.
const CONFIG_EXAMPLE_CONTENT: &str = r#"# Forgejo Runner Orchestrator - Example Configuration
# Copy this file to config.toml and customize the values.

[forgejo]
# URL of your Forgejo instance
url = "https://forgejo.example.com"
# Personal Access Token for API authentication
token = "your-forgejo-token"
# Optional: only spin up a runner when a waiting task has one of these labels.
# Remove or leave empty to react to all waiting tasks.
# Note: Forgejo may not expose runner labels via the tasks API — leave unset if unsure.
# tag_filter = ["self-hosted", "my-runner-label"]

[hetzner]
# Hetzner Cloud API Token
token = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
# Server type (e.g., cx22, ccx23, cpx31)
server_type = "ccx23"
# Datacenter location (nbg1, fsn1, hel1, ash, hil)
location = "nbg1"
# OS Image
image = "ubuntu-24.04"
# Name of the SSH key in Hetzner Cloud
ssh_key_name = "my-ssh-key"

[runner]
# Name of the server in Hetzner Cloud
name = "flexi-runner"
# Minimum runtime in minutes before the server can be deleted
min_lifetime_minutes = 20
# Polling interval in seconds
poll_interval_seconds = 30
"#;

/// Returns true if compiled in debug mode.
/// In debug mode, the server is immediately deleted when no pipelines are active.
const fn is_debug_build() -> bool {
    cfg!(debug_assertions)
}

/// Creates the example configuration if it doesn't exist.
fn ensure_example_config() {
    if !Path::new(CONFIG_EXAMPLE_PATH).exists() {
        // Create config directory if needed
        std::fs::create_dir_all("config").ok();

        if let Err(e) = std::fs::write(CONFIG_EXAMPLE_PATH, CONFIG_EXAMPLE_CONTENT) {
            eprintln!("Warning: Could not create example configuration: {}", e);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create example configuration if not present
    ensure_example_config();

    // Create log directory if not present
    std::fs::create_dir_all(LOG_DIR).ok();

    // File appender for rotating logs (daily)
    let file_appender = RollingFileAppender::new(Rotation::DAILY, LOG_DIR, "orchestrator.log");

    // Initialize tracing with console + file output
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        // Console layer - compact format
        .with(
            fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        // File layer - detailed format with timestamps
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false) // No ANSI colors in file
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
        )
        .init();

    info!("=== Forgejo Runner Orchestrator ===");
    info!("Logs are written to: {}/orchestrator.log", LOG_DIR);
    if is_debug_build() {
        warn!("DEBUG BUILD: Server will be deleted immediately when no tasks are active!");
    }
    info!("Starting...");

    // Check if config directory exists
    if !Path::new("config").exists() {
        error!("Config directory 'config/' does not exist!");
        error!("Please create config/config.toml and config/data/.runner");
        std::process::exit(1);
    }

    // Load configuration
    let config = Config::load(CONFIG_PATH).context("Error loading configuration")?;

    // Load pre-registered runner credentials
    let runner_config = load_runner_config(RUNNER_CONFIG_PATH)
        .context("Error loading runner credentials (.runner)")?;

    // Initialize CSV logger
    let csv_logger = CsvLogger::new(LOG_DIR).context("Error initializing CSV logger")?;

    // Create API clients
    let forgejo_client = ForgejoClient::new(&config.forgejo);
    let hetzner_client = HetznerClient::new(&config.hetzner);

    // Generate cloud-init template
    let cloud_init = generate_cloud_init(&runner_config);

    // Load orchestrator state with persistence
    let mut state =
        OrchestratorState::with_persistence(STATE_PATH).context("Error loading state")?;

    // Verify that saved state matches Hetzner
    verify_state_with_hetzner(&hetzner_client, &config.runner.name, &mut state).await?;

    // Polling interval (Debug: 5s, Release: from config)
    let poll_interval_secs = if is_debug_build() {
        5
    } else {
        config.runner.poll_interval_seconds
    };
    let poll_interval = Duration::from_secs(poll_interval_secs);

    info!("Starting polling loop (interval: {}s)", poll_interval_secs);
    info!(
        "Minimum server runtime: {} minutes",
        config.runner.min_lifetime_minutes
    );

    // Main loop
    loop {
        if let Err(e) = orchestration_tick(
            &forgejo_client,
            &hetzner_client,
            &csv_logger,
            &cloud_init,
            &config,
            &mut state,
        )
        .await
        {
            error!("Error in orchestration tick: {}", e);
            // Continue on errors
        }

        sleep(poll_interval).await;
    }
}

/// Verifies that the saved state matches Hetzner.
///
/// Possible scenarios:
/// - State says server exists, Hetzner too → OK, keep state
/// - State says server exists, Hetzner doesn't → Clear state
/// - State says no server, Hetzner has one → Update state (emergency)
/// - Both say no server → OK
async fn verify_state_with_hetzner(
    hetzner_client: &HetznerClient,
    server_name: &str,
    state: &mut OrchestratorState,
) -> Result<()> {
    info!("Verifying state with Hetzner API...");

    let hetzner_server = hetzner_client.find_server_by_name(server_name).await?;

    match (&state.runner, hetzner_server) {
        // State and Hetzner match - server exists
        (Some(runner), Some(server)) if runner.server_id == server.id => {
            info!(
                "State verified: Server {} (ID: {}) exists, running for {} minutes",
                server.name,
                server.id,
                runner.uptime_minutes()
            );
        }

        // State says server exists, but Hetzner doesn't know it anymore
        (Some(runner), None) => {
            warn!(
                "State inconsistency: Server {} (ID: {}) no longer exists at Hetzner!",
                runner.server_name, runner.server_id
            );
            warn!("Clearing state...");
            state.clear_runner();
        }

        // State says server exists, but with different ID (very unlikely)
        (Some(runner), Some(server)) => {
            warn!(
                "State inconsistency: State knows server ID {}, Hetzner has ID {}!",
                runner.server_id, server.id
            );
            warn!("Updating state with Hetzner data (creation time unknown)...");
            let new_runner = RunnerState::new(server.id, server.name);
            state.set_runner(new_runner);
        }

        // State says no server, but Hetzner has one (emergency recovery)
        (None, Some(server)) => {
            warn!(
                "Orphaned server found: {} (ID: {}) - not in state!",
                server.name, server.id
            );
            warn!("Adding to state (creation time unknown)...");
            let new_runner = RunnerState::new(server.id, server.name);
            state.set_runner(new_runner);
        }

        // All OK - no server
        (None, None) => {
            info!("No server active - state is consistent");
        }
    }

    Ok(())
}

/// One pass of the orchestration logic.
async fn orchestration_tick(
    forgejo_client: &ForgejoClient,
    hetzner_client: &HetznerClient,
    csv_logger: &CsvLogger,
    cloud_init: &str,
    config: &Config,
    state: &mut OrchestratorState,
) -> Result<()> {
    // Query Forgejo for active tasks (filtered by label if configured)
    let active_jobs = forgejo_client
        .find_active_jobs(config.forgejo.tag_filter.as_deref())
        .await?;
    let has_active = !active_jobs.is_empty();

    if has_active {
        // There are active tasks - ensure server exists
        if !state.has_runner() {
            // No server present - create one
            let first_job = &active_jobs[0];
            info!(
                "Active task found: {} (ID: {}) in {}",
                first_job.job.status, first_job.job.id, first_job.project.full_name
            );

            create_runner(
                hetzner_client,
                csv_logger,
                cloud_init,
                config,
                state,
                &first_job.project.full_name,
                first_job.job.id,
            )
            .await?;
        } else {
            // Server is already running
            if let Some(uptime) = state.runner_uptime() {
                info!(
                    "Server running ({}min), {} active job(s)",
                    uptime,
                    active_jobs.len()
                );
            }
        }
    } else {
        // No active jobs
        if state.has_runner() {
            // Server is running, but no jobs anymore - check if we should delete
            maybe_delete_runner(hetzner_client, csv_logger, config, state).await?;
        } else {
            info!("No active jobs, no server active - waiting...");
        }
    }

    Ok(())
}

/// Creates a new runner server.
async fn create_runner(
    hetzner_client: &HetznerClient,
    csv_logger: &CsvLogger,
    cloud_init: &str,
    config: &Config,
    state: &mut OrchestratorState,
    project: &str,
    pipeline_id: u64,
) -> Result<()> {
    info!("Creating new runner server...");

    let server = hetzner_client
        .create_server(&config.runner.name, cloud_init)
        .await
        .context("Error creating server")?;

    // Update state
    let runner_state = RunnerState::new(server.id, server.name.clone());
    state.set_runner(runner_state);

    // CSV log
    if let Err(e) = csv_logger.log_start(server.id, project, pipeline_id, "pipeline_pending") {
        warn!("Error in CSV logging: {}", e);
    }

    info!("Runner server created and ready");
    Ok(())
}

/// Checks if the server should be deleted and performs deletion if so.
///
/// In debug build, the server is deleted immediately.
/// In release build, it waits for optimal billing time.
async fn maybe_delete_runner(
    hetzner_client: &HetznerClient,
    csv_logger: &CsvLogger,
    config: &Config,
    state: &mut OrchestratorState,
) -> Result<()> {
    let runner = match &state.runner {
        Some(r) => r,
        None => return Ok(()),
    };

    let uptime = runner.uptime_minutes();

    // DEBUG BUILD: Delete immediately without waiting for billing
    if is_debug_build() {
        info!(
            "[DEBUG] Server running for {}min - deleting immediately (no pipelines active)",
            uptime
        );
        delete_runner(hetzner_client, csv_logger, state, "debug_immediate_delete").await?;
        return Ok(());
    }

    // RELEASE BUILD: Wait for optimal billing time
    let min_lifetime = config.runner.min_lifetime_minutes;
    let minutes_to_billing = runner.minutes_until_next_billing_cycle();

    // Check if optimal delete time
    let should_delete = runner.should_delete(min_lifetime, BILLING_BUFFER_MINUTES);

    // Or: Minimum runtime reached and we don't want to wait forever
    // (e.g., when we're just past a full hour)
    let can_force_delete = runner.can_force_delete(min_lifetime);

    if should_delete {
        // Optimal time - delete
        delete_runner(hetzner_client, csv_logger, state, "optimal_billing_time").await?;
    } else if can_force_delete && minutes_to_billing > 55 {
        // We're just past a full hour and minimum runtime is reached
        // Deleting makes sense, otherwise we'd wait almost a full hour
        info!(
            "Force-delete: Minimum runtime reached ({}min), {} minutes until billing",
            uptime, minutes_to_billing
        );
        delete_runner(hetzner_client, csv_logger, state, "all_pipelines_done").await?;
    } else {
        // Wait some more
        info!(
            "Server running for {}min, no pipelines active. {} minutes until billing - waiting...",
            uptime, minutes_to_billing
        );
    }

    Ok(())
}

/// Deletes the runner server.
async fn delete_runner(
    hetzner_client: &HetznerClient,
    csv_logger: &CsvLogger,
    state: &mut OrchestratorState,
    reason: &str,
) -> Result<()> {
    let runner = match &state.runner {
        Some(r) => r,
        None => return Ok(()),
    };

    let server_id = runner.server_id;
    let uptime = runner.uptime_minutes();

    info!("Deleting runner server (reason: {})", reason);

    // Delete server
    hetzner_client
        .delete_server(server_id)
        .await
        .context("Error deleting server")?;

    // CSV log
    if let Err(e) = csv_logger.log_stop(server_id, reason, uptime) {
        warn!("Error in CSV logging: {}", e);
    }

    // Reset state
    state.clear_runner();

    info!("Runner server deleted (runtime: {} minutes)", uptime);
    Ok(())
}
