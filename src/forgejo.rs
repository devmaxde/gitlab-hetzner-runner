//! Forgejo API client for Actions job status queries.

use std::collections::HashMap;

use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, info};

use crate::config::ForgejoConfig;

#[derive(Error, Debug)]
pub enum ForgejoError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Forgejo API error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("Invalid API response: {0}")]
    Parse(String),
}

/// A Forgejo repository with its numeric ID and full name.
#[derive(Debug, Deserialize, Clone)]
struct RepoInfo {
    id: u64,
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct ReposSearchResponse {
    data: Vec<RepoInfo>,
}

/// An ActionRunJob as returned by `/admin/runners/jobs`.
#[derive(Debug, Deserialize)]
struct AdminRunnerJob {
    id: u64,
    repo_id: u64,
    /// Labels from `runs-on` in the workflow file.
    #[serde(default)]
    runs_on: Vec<String>,
    status: String,
}

// ── Public types used by main.rs ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Repo {
    pub full_name: String,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: u64,
    pub status: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveJob {
    pub project: Repo,
    pub job: Task,
}

// ── Client ───────────────────────────────────────────────────────────────────

pub struct ForgejoClient {
    client: Client,
    base_url: String,
    token: String,
}

impl ForgejoClient {
    pub fn new(config: &ForgejoConfig) -> Self {
        let client = Client::new();
        let base_url = config.url.trim_end_matches('/').to_string();
        info!("Forgejo client initialized for: {}", base_url);
        Self {
            client,
            base_url,
            token: config.token.clone(),
        }
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T, ForgejoError> {
        let url = format!("{}/api/v1{}", self.base_url, endpoint);
        debug!("Forgejo API GET: {}", url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(ForgejoError::Api {
                status: status.as_u16(),
                message,
            });
        }

        response
            .json::<T>()
            .await
            .map_err(|e| ForgejoError::Parse(format!("JSON parsing failed: {}", e)))
    }

    /// Builds a map of repo_id → full_name from the paginated repo search.
    async fn repo_id_map(&self) -> Result<HashMap<u64, String>, ForgejoError> {
        let mut map = HashMap::new();
        let mut page = 1;
        let limit = 50;

        loop {
            let endpoint = format!("/repos/search?limit={}&page={}", limit, page);
            let response: ReposSearchResponse = self.get(&endpoint).await?;
            let count = response.data.len();
            for repo in response.data {
                map.insert(repo.id, repo.full_name);
            }
            if count < limit {
                break;
            }
            page += 1;
        }

        debug!("{} repos indexed", map.len());
        Ok(map)
    }

    /// Fetches all runner jobs from the admin endpoint.
    ///
    /// Returns `null` when there are no jobs — modelled as `Option<Vec<_>>`.
    async fn admin_runner_jobs(&self) -> Result<Vec<AdminRunnerJob>, ForgejoError> {
        let jobs: Option<Vec<AdminRunnerJob>> = self.get("/admin/runners/jobs").await?;
        Ok(jobs.unwrap_or_default())
    }

    /// Returns all active (waiting or running) jobs across every repo.
    ///
    /// When `tag_filter` is set, only jobs whose `runs_on` labels include at
    /// least one of the configured tags are returned.
    pub async fn find_active_jobs(
        &self,
        tag_filter: Option<&[String]>,
    ) -> Result<Vec<ActiveJob>, ForgejoError> {
        if let Some(tags) = tag_filter {
            debug!("Tag filter active: {:?}", tags);
        }

        let repos = self.repo_id_map().await?;
        let all_jobs = self.admin_runner_jobs().await?;
        let mut active_jobs = Vec::new();

        for job in all_jobs {
            if job.status != "waiting" && job.status != "running" {
                continue;
            }

            if let Some(tags) = tag_filter {
                if !job.runs_on.iter().any(|l| tags.contains(l)) {
                    debug!(
                        "Skipping job {} (runs_on: {:?} don't match filter)",
                        job.id, job.runs_on
                    );
                    continue;
                }
            }

            let full_name = repos
                .get(&job.repo_id)
                .cloned()
                .unwrap_or_else(|| format!("repo/{}", job.repo_id));

            debug!(
                "Active job: {} (status: {}) in {}",
                job.id, job.status, full_name
            );
            active_jobs.push(ActiveJob {
                project: Repo { full_name },
                job: Task {
                    id: job.id,
                    status: job.status,
                    labels: job.runs_on,
                },
            });
        }

        if active_jobs.is_empty() {
            info!("No active jobs found");
        } else {
            info!("{} active job(s) found", active_jobs.len());
        }

        Ok(active_jobs)
    }
}
