//! Configuration module for the Forgejo Runner Orchestrator.
//!
//! Loads configuration from `config/config.toml` and provides
//! typed structs for all settings.

use serde::Deserialize;
use std::path::Path;
use thiserror::Error;
use tracing::info;

/// Errors that can occur when loading configuration.
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read configuration file: {0}")]
    Read(#[from] std::io::Error),

    #[error("Failed to parse configuration: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("Failed to read runner credentials (.runner): {0}")]
    RunnerConfig(std::io::Error),
}

/// Main configuration - contains all sub-configurations.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub forgejo: ForgejoConfig,
    pub hetzner: HetznerConfig,
    pub runner: RunnerConfig,
}

/// Forgejo-specific configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct ForgejoConfig {
    /// URL of the Forgejo instance (e.g., "https://forgejo.example.com")
    pub url: String,
    /// Personal Access Token for API authentication
    pub token: String,
    /// Optional runner label filter - only spin up a runner when a waiting task has one of these labels.
    /// If absent, all waiting tasks trigger a runner (original behavior).
    /// Note: Forgejo may not expose runner labels via the tasks API — leave unset if unsure.
    pub tag_filter: Option<Vec<String>>,
}

/// Hetzner Cloud configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct HetznerConfig {
    /// Hetzner API Token
    pub token: String,
    /// Server type (e.g., "ccx23" for AMD dedicated CPU)
    pub server_type: String,
    /// Datacenter location (e.g., "nbg1", "fsn1", "hel1")
    pub location: String,
    /// OS Image (e.g., "ubuntu-24.04")
    pub image: String,
    /// Name of the SSH key in Hetzner Cloud
    pub ssh_key_name: String,
}

/// Runner-specific configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct RunnerConfig {
    /// Name of the server in Hetzner Cloud
    pub name: String,
    /// Minimum runtime in minutes before the server can be deleted
    #[serde(default = "default_min_lifetime")]
    pub min_lifetime_minutes: u32,
    /// Polling interval in seconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
}

/// Default value for minimum lifetime: 20 minutes
fn default_min_lifetime() -> u32 {
    20
}

/// Default value for polling interval: 30 seconds
fn default_poll_interval() -> u64 {
    30
}

impl Config {
    /// Loads configuration from the specified file.
    ///
    /// # Arguments
    /// * `path` - Path to the config.toml file
    ///
    /// # Returns
    /// The loaded configuration or an error
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        info!("Loading configuration from: {}", path.display());

        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;

        info!("Configuration loaded successfully");
        info!("  Forgejo URL: {}", config.forgejo.url);
        info!("  Hetzner server type: {}", config.hetzner.server_type);
        info!("  Runner name: {}", config.runner.name);

        Ok(config)
    }

    // NOTE: `load_default()` was removed - not needed in this context,
    // as the config path is explicitly defined in main.rs.
}

/// Loads the pre-registered `.runner` credentials file for cloud-init injection.
///
/// # Arguments
/// * `path` - Path to the `.runner` file (e.g., `config/data/.runner`)
pub fn load_runner_config<P: AsRef<Path>>(path: P) -> Result<String, ConfigError> {
    let path = path.as_ref();
    info!("Loading runner credentials from: {}", path.display());

    std::fs::read_to_string(path).map_err(ConfigError::RunnerConfig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        assert_eq!(default_min_lifetime(), 20);
        assert_eq!(default_poll_interval(), 30);
    }
}
