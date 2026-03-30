//! Cloud-Init template generator for the Forgejo runner server.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use tracing::info;

/// Docker Compose configuration — embedded at compile time.
const DOCKER_COMPOSE: &str = include_str!("../assets/docker-compose.yml");

/// Generates the cloud-init configuration for the runner server.
///
/// The configuration:
/// 1. Updates packages
/// 2. Writes the pre-registered `.runner` credentials to `data/.runner`
/// 3. Writes the docker-compose.yml
/// 4. Sets ownership of the data directory to uid/gid 1001 (forgejo-runner)
/// 5. Installs Docker
/// 6. Starts the Forgejo runner via docker compose
///
/// # Arguments
/// * `runner_dot_file` - Contents of the pre-registered `config/data/.runner` file
pub fn generate_cloud_init(runner_dot_file: &str) -> String {
    info!("Generating cloud-init configuration");

    let runner_b64 = BASE64.encode(runner_dot_file.as_bytes());
    let docker_compose_b64 = BASE64.encode(DOCKER_COMPOSE.as_bytes());

    let cloud_init = format!(
        r#"#cloud-config
package_update: true
package_upgrade: true

write_files:
  - path: /srv/forgejo-runner/docker-compose.yml
    encoding: b64
    content: {docker_compose_b64}
  - path: /srv/forgejo-runner/data/.runner
    encoding: b64
    content: {runner_b64}
    owner: '1001:1001'
    permissions: '0600'

runcmd:
  - chown -R 1001:1001 /srv/forgejo-runner/data
  - curl -fsSL https://get.docker.com -o install-docker.sh
  - sh install-docker.sh
  - docker compose -f /srv/forgejo-runner/docker-compose.yml up -d
"#
    );

    info!(
        "Cloud-init configuration generated ({} bytes)",
        cloud_init.len()
    );
    cloud_init
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_cloud_init() {
        let result = generate_cloud_init(r#"{"id":"test"}"#);

        assert!(result.starts_with("#cloud-config"));
        assert!(result.contains("package_update: true"));
        assert!(result.contains("/srv/forgejo-runner/data/.runner"));
        assert!(result.contains("forgejo-runner"));
        assert!(result.contains("docker compose"));
    }
}
