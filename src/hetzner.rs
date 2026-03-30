//! Hetzner Cloud API client for server management.
//!
//! Uses raw HTTP requests for server creation, deletion, and status queries.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info};

use crate::config::HetznerConfig;

/// Hetzner API base URL
const HETZNER_API_URL: &str = "https://api.hetzner.cloud/v1";

/// Errors that can occur during Hetzner API calls.
///
/// NOTE: `ServerNotFound` variant was removed - not needed in this context,
/// as server existence is handled via Option<Server>.
#[derive(Error, Debug)]
pub enum HetznerError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Hetzner API error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("SSH key not found: {0}")]
    SshKeyNotFound(String),

    #[error("Invalid API response: {0}")]
    Parse(String),
}

/// Hetzner server information.
///
/// NOTE: Additional fields like `created`, `server_type`, `datacenter` etc. were removed -
/// not needed in this context.
#[derive(Debug, Deserialize, Clone)]
pub struct Server {
    /// Server ID
    pub id: u64,
    /// Server name
    pub name: String,
    /// Server status (initializing, running, off, etc.)
    pub status: String,
    /// Public IPv4 address
    pub public_net: PublicNet,
}

/// Network information of a server.
///
/// NOTE: `ipv6` was removed - not needed in this context.
#[derive(Debug, Deserialize, Clone)]
pub struct PublicNet {
    pub ipv4: Option<Ipv4>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Ipv4 {
    pub ip: String,
}

/// SSH key information.
///
/// NOTE: `name` and other fields were removed - only `id` is needed.
#[derive(Debug, Deserialize, Clone)]
pub struct SshKey {
    pub id: u64,
}

/// Request body for server creation.
#[derive(Debug, Serialize)]
struct CreateServerRequest {
    name: String,
    server_type: String,
    image: String,
    location: String,
    ssh_keys: Vec<u64>,
    user_data: String,
}

/// API response for server creation.
#[derive(Debug, Deserialize)]
struct CreateServerResponse {
    server: Server,
}

/// API response for server list.
#[derive(Debug, Deserialize)]
struct ServersResponse {
    servers: Vec<Server>,
}

/// API response for SSH key list.
#[derive(Debug, Deserialize)]
struct SshKeysResponse {
    ssh_keys: Vec<SshKey>,
}

/// Hetzner API client.
pub struct HetznerClient {
    /// HTTP client
    client: Client,
    /// API token
    token: String,
    /// Server configuration
    config: HetznerConfig,
}

impl HetznerClient {
    /// Creates a new Hetzner client.
    ///
    /// # Arguments
    /// * `config` - Hetzner configuration with token and server settings
    pub fn new(config: &HetznerConfig) -> Self {
        let client = Client::new();

        info!("Hetzner client initialized");
        info!("  Server type: {}", config.server_type);
        info!("  Location: {}", config.location);
        info!("  Image: {}", config.image);

        Self {
            client,
            token: config.token.clone(),
            config: config.clone(),
        }
    }

    /// Executes an authenticated GET request.
    async fn get<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T, HetznerError> {
        let url = format!("{}{}", HETZNER_API_URL, endpoint);
        debug!("Hetzner API GET: {}", url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(HetznerError::Api {
                status: status.as_u16(),
                message,
            });
        }

        response
            .json::<T>()
            .await
            .map_err(|e| HetznerError::Parse(format!("JSON parsing failed: {}", e)))
    }

    /// Executes an authenticated POST request.
    async fn post<T: for<'de> Deserialize<'de>, B: Serialize>(
        &self,
        endpoint: &str,
        body: &B,
    ) -> Result<T, HetznerError> {
        let url = format!("{}{}", HETZNER_API_URL, endpoint);
        debug!("Hetzner API POST: {}", url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(HetznerError::Api {
                status: status.as_u16(),
                message,
            });
        }

        response
            .json::<T>()
            .await
            .map_err(|e| HetznerError::Parse(format!("JSON parsing failed: {}", e)))
    }

    /// Executes an authenticated DELETE request.
    async fn delete(&self, endpoint: &str) -> Result<(), HetznerError> {
        let url = format!("{}{}", HETZNER_API_URL, endpoint);
        debug!("Hetzner API DELETE: {}", url);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(HetznerError::Api {
                status: status.as_u16(),
                message,
            });
        }

        Ok(())
    }

    /// Gets the ID of an SSH key by name.
    pub async fn get_ssh_key_id(&self, name: &str) -> Result<u64, HetznerError> {
        let endpoint = format!("/ssh_keys?name={}", name);
        let response: SshKeysResponse = self.get(&endpoint).await?;

        response
            .ssh_keys
            .first()
            .map(|key| key.id)
            .ok_or_else(|| HetznerError::SshKeyNotFound(name.to_string()))
    }

    /// Finds a server by name.
    ///
    /// # Returns
    /// * `Ok(Some(Server))` - Server found
    /// * `Ok(None)` - No server with this name
    pub async fn find_server_by_name(&self, name: &str) -> Result<Option<Server>, HetznerError> {
        let endpoint = format!("/servers?name={}", name);
        let response: ServersResponse = self.get(&endpoint).await?;

        Ok(response.servers.into_iter().next())
    }

    // NOTE: `server_exists()` was removed - not needed in this context,
    // as `find_server_by_name()` already returns Option<Server>.

    /// Creates a new server.
    ///
    /// # Arguments
    /// * `name` - Name of the server
    /// * `user_data` - Cloud-init configuration (base64-encoded or plain)
    ///
    /// # Returns
    /// The created server with ID and information
    pub async fn create_server(&self, name: &str, user_data: &str) -> Result<Server, HetznerError> {
        info!("Creating server: {}", name);
        // Get SSH key ID
        let ssh_key_id = self.get_ssh_key_id(&self.config.ssh_key_name).await?;
        debug!("SSH Key ID: {}", ssh_key_id);

        let request = CreateServerRequest {
            name: name.to_string(),
            server_type: self.config.server_type.clone(),
            image: self.config.image.clone(),
            location: self.config.location.clone(),
            ssh_keys: vec![ssh_key_id],
            user_data: user_data.to_string(),
        };

        let response: CreateServerResponse = self.post("/servers", &request).await?;

        info!(
            "Server created: {} (ID: {}, status: {})",
            response.server.name, response.server.id, response.server.status
        );

        if let Some(ref ipv4) = response.server.public_net.ipv4 {
            info!("  IPv4: {}", ipv4.ip);
        }

        Ok(response.server)
    }

    /// Deletes a server by its ID.
    ///
    /// # Arguments
    /// * `server_id` - ID of the server to delete
    pub async fn delete_server(&self, server_id: u64) -> Result<(), HetznerError> {
        info!("Deleting server with ID: {}", server_id);

        let endpoint = format!("/servers/{}", server_id);
        self.delete(&endpoint).await?;

        info!("Server {} successfully deleted", server_id);
        Ok(())
    }

    // NOTE: `delete_server_by_name()` and `get_server_status()` were removed -
    // not needed in this context, as server ID is stored in state.
}
