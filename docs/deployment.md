# Deployment

## Choose a transport

| Situation | Transport | Why |
| --- | --- | --- |
| Agent runs on the same machine as the printer/server | stdio | No network surface, no token to manage |
| Agent runs in a desktop app that launches processes | stdio | The client manages the lifecycle |
| Agent runs in a container or remote host | HTTP | The client connects over the network |
| Multiple clients share one server | HTTP | One long-lived process |

stdio remains the recommended default. HTTP is for clients that cannot spawn a process.

## Local install

```bash
cargo install --path .
```

The binary lands in `~/.cargo/bin/mcp-moonraker`. Verify:

```bash
mcp-moonraker --version
mcp-moonraker doctor
```

For a system-wide install, copy the binary to `/usr/local/bin` or use a package manager.
The binary has no runtime assets and no config file is required.

## systemd (stdio not applicable)

A stdio server is launched by the agent, so it should not be a system service. Run HTTP
mode under systemd when a remote agent needs a persistent endpoint.

`/etc/mcp-moonraker.toml`:

```toml
[moonraker]
url = "http://127.0.0.1:7125"

[server]
transport = "http"

[server.http]
host = "127.0.0.1"
port = 8765

[approval]
mode = "out_of_band"
state_dir = "/var/lib/mcp-moonraker"

[logging]
level = "info"
audit_log = "/var/log/mcp-moonraker/audit.jsonl"
```

`/etc/systemd/system/mcp-moonraker.service`:

```ini
[Unit]
Description=mcp-moonraker server
After=network-online.target moonraker.service
Wants=network-online.target

[Service]
Type=simple
User=mcp-moonraker
Group=mcp-moonraker
EnvironmentFile=/etc/mcp-moonraker.env
ExecStart=/usr/local/bin/mcp-moonraker --config /etc/mcp-moonraker.toml serve
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/mcp-moonraker /var/log/mcp-moonraker
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
```

`/etc/mcp-moonraker.env` (mode `0600`, owned by the service user):

```bash
MOONRAKER_API_KEY=...
MCP_MOONRAKER_HTTP_TOKEN=...
```

The token is only needed if the bind address is non-loopback. With the config above, bind
to `127.0.0.1` and put a reverse proxy in front, or set `host = "0.0.0.0"` and let the
startup validation require the token.

```bash
sudo useradd --system --home /var/lib/mcp-moonraker --shell /usr/sbin/nologin mcp-moonraker
sudo install -d -o mcp-moonraker -g mcp-moonraker -m 0700 /var/lib/mcp-moonraker
sudo install -d -o mcp-moonraker -g mcp-moonraker -m 0750 /var/log/mcp-moonraker
sudo systemctl daemon-reload
sudo systemctl enable --now mcp-moonraker
curl -fsS http://127.0.0.1:8765/health
```

Human approvals:

```bash
sudo -u mcp-moonraker mcp-moonraker --config /etc/mcp-moonraker.toml pending
sudo -u mcp-moonraker mcp-moonraker --config /etc/mcp-moonraker.toml approve apr_...
```

The CLI must use the same `state_dir` as the server. `EnvironmentFile` is not loaded by the
CLI unless you pass the same environment; prefer putting `state_dir` in the config file.

## Docker

Build:

```bash
docker build -t mcp-moonraker .
```

stdio in a container is awkward because the client must attach to the process's stdio. Use
HTTP:

```bash
docker run --rm -p 8765:8765 \
  -e MOONRAKER_URL=http://192.168.1.50:7125 \
  -e MCP_MOONRAKER_HTTP_TOKEN="$(openssl rand -hex 32)" \
  -e MCP_MOONRAKER_HTTP_HOST=0.0.0.0 \
  -v mcp-moonraker-state:/var/lib/mcp-moonraker \
  -e MCP_MOONRAKER_STATE_DIR=/var/lib/mcp-moonraker \
  mcp-moonraker
```

The image defaults to HTTP on `0.0.0.0:8765` and refuses to start without a token unless
you explicitly allow unauthenticated access. `127.0.0.1` inside the container is the
container itself; use the host's LAN address, `host.docker.internal` (Docker Desktop), or
a Docker network alias for Moonraker.

Do not put the API key or token in the image or a committed compose file. Use an env file
with restrictive permissions or a secret manager.

`examples/docker-compose.yml` shows a complete setup with a health check.

## Reverse proxy and TLS

The server does not terminate TLS. A minimal Caddy configuration:

```caddyfile
mcp.example.com {
    reverse_proxy 127.0.0.1:8765
}
```

Caddy obtains and renews certificates automatically. If you use nginx, disable buffering and
set generous read timeouts for SSE:

```nginx
location /mcp {
    proxy_pass http://127.0.0.1:8765;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;
    proxy_cache off;
    proxy_read_timeout 1h;
    proxy_send_timeout 1h;
}

location /health {
    proxy_pass http://127.0.0.1:8765/health;
}
```

Set `server.http.allowed_hosts` to the public hostname and `server.http.allowed_origins` to
the browser origins that need access. DNS-rebinding protection otherwise rejects unfamiliar
`Host` headers. Add rate limiting and authentication at the proxy if more than one trusted
client uses the endpoint.

Never expose the HTTP port directly to the internet. The static bearer token is suitable
for a trusted LAN or a TLS reverse proxy, not as an internet-facing authorization server.

## Client examples

### Claude Desktop

```json
{
  "mcpServers": {
    "moonraker": {
      "command": "/home/you/.cargo/bin/mcp-moonraker",
      "env": {
        "MOONRAKER_URL": "http://192.168.1.50:7125"
      }
    }
  }
}
```

For HTTP clients that support remote MCP servers, use
`https://mcp.example.com/mcp` with an `Authorization: Bearer <token>` header.

### OpenCode

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "moonraker": {
      "type": "local",
      "command": ["mcp-moonraker"],
      "enabled": true,
      "environment": {
        "MOONRAKER_URL": "http://192.168.1.50:7125"
      }
    }
  }
}
```

### Agent instructions

The server advertises an `instructions` string explaining the approval flow. Add your own
policy in the agent's system prompt if needed, for example:

> Never call a destructive tool without asking me first, even if the MCP server would
> accept it. Use read-only tools to gather context, then propose the action.

This is a social control on top of the server's technical controls.

## Multiple printers

Run one server process per printer. Each process needs its own `MOONRAKER_URL`, HTTP port,
and state directory. Do not share a state directory between instances: approval IDs are
unique, but `pending` would mix records and the TTLs are per-server.

## Upgrades

1. Stop the service or let the client restart the process.
2. Build or install the new binary.
3. Run `mcp-moonraker doctor` with the production config.
4. Restart.

Pending approvals survive a restart because they are files. Consumed or expired records are
harmless and can be pruned by deleting old JSON files in `<state_dir>/approvals/` while the
server is stopped.

## Resource use

The server is a small async process. Memory is dominated by transient Moonraker responses
and camera frames, both capped by configuration. There is no database; the only persistent
state is the approval directory and the optional audit log. Rotate the audit log with your
normal log tooling.
