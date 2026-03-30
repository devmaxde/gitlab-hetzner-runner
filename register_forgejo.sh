FORGEJO_URL="${1:?Usage: $0 <forgejo-url> <register-token>}"
REGISTER_TOKEN="${2:?Usage: $0 <forgejo-url> <register-token>}"

mkdir data
chown -R 1001:1001 data
docker run --rm \
  -v "$(pwd)/data:/data" \
  --user 1001:1001 \
  data.forgejo.org/forgejo/runner:12 \
  forgejo-runner register \
    --instance ${FORGEJO_URL} \
    --token ${REGISTER_TOKEN} \
    --name "autorunner" \
    --labels "docker:docker://node:20-bookworm,ubuntu-latest:docker://node:20-bookworm,ubuntu-22.04:docker://node:20-bookworm" \
    --no-interactive

cat > docker-compose.yml << 'EOF'
services:
  docker-in-docker:
    image: docker:dind
    container_name: 'docker_dind'
    privileged: 'true'
    command: ['dockerd', '-H', 'tcp://0.0.0.0:2375', '--tls=false']
    restart: 'unless-stopped'

  runner:
    image: 'data.forgejo.org/forgejo/runner:12'
    links:
      - docker-in-docker
    depends_on:
      docker-in-docker:
        condition: service_started
    container_name: 'runner'
    environment:
      DOCKER_HOST: tcp://docker-in-docker:2375
    user: 1001:1001
    volumes:
      - ./data:/data
    restart: 'unless-stopped'
    command: 'forgejo-runner daemon'
EOF

docker compose up -d
