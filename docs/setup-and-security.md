# Real-world setup and Moonraker security

This guide is for the moment things stop working on a real printer: the MCP server
starts, OpenCode calls a tool, and Moonraker never sees a request. It covers the
whole chain — how OpenCode launches this server, how to pick `MOONRAKER_URL` for
each network topology, how to configure Moonraker's `[authorization]` to accept the
server, and how to tell apart *network* failures from *authorization* failures.

Read the three numbered steps in order for a fresh install; use the
[diagnostics checklist](#diagnostics-checklist) and the
[troubleshooting matrix](#troubleshooting-matrix) when debugging an existing one.

## How the pieces talk

```
+------------------+    stdio     +------------------+    HTTP     +------------------+
|     OpenCode     |  ----------> |  mcp-moonraker   |  ---------> |     Moonraker    |
|  (MCP client)    |  JSON-RPC    |   (MCP server)   |   POST      |   HTTP :7125     |
+------------------+              +------------------+  /server/   +------------------+
                                        started BY              +---- Klipper (UDS)
                                        YOUR SHELL/OPENCODE            /tmp/klippy_uds
```

Three facts follow from this picture, and they explain most "it never reaches the
printer" reports:

1. **OpenCode launches `mcp-moonraker` itself** (stdio transport). The process is
   not started from an interactive shell — it inherits whatever environment
   OpenCode passes in the `environment` block of `opencode.json`, which is usually
   *much* smaller than your shell's. If `MOONRAKER_URL` is not set there or in a
   TOML config file, the server silently falls back to `http://127.0.0.1:7125`.
2. **There is no connectivity check at startup.** The server only contacts
   Moonraker the first time a tool is called, so a wrong URL or a missing API key
   never fails loudly — you just see a per-call error, or (worse) an agent that
   says "the printer is unreachable" without showing the underlying cause.
3. **Moonraker is a plain HTTP service** (default `0.0.0.0:7125`), not a browser
   frontend. If you normally open the printer at `http://printer.local/` (port 80)
   you are going through nginx; Moonraker itself is on port 7125.

## Step 1 — Install and verify standalone

```bash
git clone https://github.com/martinsgmx/mcp-moonraker.git
cd mcp-moonraker
cargo install --path .          # binary lands in ~/.cargo/bin/
mcp-moonraker --version         # must print, calls nothing
mcp-moonraker doctor            # probes Moonraker, Klipper, cameras, approvals
```

`doctor` prints one line per check and fails fast on the first real problem:

- `FAIL Moonraker — unreachable: ...` → the URL or the network is wrong (see
  [Step 2](#step-2--choose-the-url-for-your-topology) and the
  [checklist](#diagnostics-checklist)).
- `Moonraker API error 401: ...` → URL and network are fine, Moonraker is refusing
  the request: missing or wrong API key (see
  [Step 3](#step-3--configure-moonraker-authorization)).

By default `doctor` uses `MOONRAKER_URL` from the environment or falls back to
`http://127.0.0.1:7125`. Override for a single run with
`mcp-moonraker doctor --url http://<printer>:7125`, or make it durable with a
config file:

```bash
mcp-moonraker init --output mcp-moonraker.toml
mcp-moonraker --config mcp-moonraker.toml doctor
```

## Step 2 — Choose the URL for your topology

Pick the row that matches where this MCP server runs relative to Moonraker:

| Where mcp-moonraker runs | `MOONRAKER_URL` | Notes |
| --- | --- | --- |
| Same host as Moonraker | `http://127.0.0.1:7125` | Works, *but only if the env var actually reaches the process* (see [Step 4](#step-4--wire-it-into-opencode)) |
| Another LAN host | `http://<printer-ip-or-host>:7125` | Use the IP unless `.local`/mDNS is configured on the host running MCP |
| Moonraker behind nginx (browser UI on port 80/443) | `http://<ui-host>/` or `https://<ui-host>/` | Do **not** append `:7125`; see below |
| Printer reachable over VPN / remote | `http://<vpn-address>:7125` | Never port-forward 7125 raw; see below |

```toml
# put this in the config file instead of relying on the environment
[moonraker]
url = "http://192.168.1.50:7125"
```

Rules that apply everywhere:

- **Never embed credentials in the URL** (`http://user:pass@host`). The server
  rejects it at startup on purpose; Moonraker-side auth is done via the API key
  (next section).
- `MOONRAKER_API_KEY` is required **only** if Moonraker demands it — don't guess.
  Run the check in [Step 5](#step-5--verify-end-to-end).

### Same host (MCP on the printer itself)

`127.0.0.1` is correct *if and only if* the MCP process runs on the same machine
as Moonraker — OpenCode spawning the server locally counts as "on the same
machine". This is the one topology where the default URL is right; still set it
explicitly so a future move to a laptop does not silently break.

### Another machine on the LAN

Set the URL to the printer's LAN address. Prefer a static IP or a `.local` name
over a DHCP address that changes. Two gotchas:

- `127.0.0.1` never works from another host — it points at the MCP host itself.
- `.local` (mDNS) names need Avahi/`libnss-mdns`-style resolution **on the host
  running the MCP server**; if `ping printer.local` fails there from a shell, the
  MCP server cannot resolve it either. Use the IP.

### Moonraker behind nginx (Mainsail / Fluidd web UI)

If the browser opens `http://printer.local/` or `https://printer.internal/` and
Moonraker lives on port 7125 behind proxy, the MCP must go through the same
front door — but **without the UI path and without the port suffix**:

| You browse as | `MOONRAKER_URL` for the MCP |
| --- | --- |
| `http://printer.local/` (nginx) | `http://printer.local/` |
| `https://printer.example.com/` | `https://printer.example.com/` (self-signed? see below) |
| `http://printer.local:7125` directly | `http://printer.local:7125` (no nginx involved) |

The server keeps any path prefix in the URL, so reverse-proxy `route_prefix`
setups (e.g. `http://host/moonraker/printer1`) work as-is. With HTTPS:

- A **self-signed certificate** (the usual setup for printer-local TLS) will fail
  verification by default. Either put a real, CA-signed cert on nginx, or set
  `moonraker.tls_verify = false` for a host you already trust — never dump the
  `https://` prefix and the verification in a way that recreates the default
  `http://127.0.0.1:7125` fallback.
- Requests proxied through nginx arrive at Moonraker from the proxy's socket;
  with Moonraker's `use_xheaders: True` (default) the client IP is taken from
  `X-Forwarded-For`. If the request carries no `X-Forwarded-For`, Moonraker sees
  the proxy (often loopback) and may treat it as trusted — which is why the API
  key sometimes "doesn't seem to be required" behind nginx for browser requests
  yet *is* required for direct `:7125` calls.

### Remote printer (different network, VPN, or routed externally)

- **Best:** WireGuard/Tailscale (or any VPN). Then the printer is just another
  private address: `http://10.x.x.x:7125`, and the API key is enough.
- **Port-forwarding Moonraker raw (`:7125`) to the internet is not safe.** Moonraker
  is plain HTTP (no TLS), the API key grants *full* access including config writes
  and host administration. If you must, put TLS + extra authentication in nginx in
  front and use the HTTPS URL above, and see
  [security checklist](#security-checklist-for-exposed-printers).
- Over plain HTTP directly on 7125 from outside, the API key is transmitted for
  anyone on that path to see.

## Step 3 — Configure Moonraker authorization

Moonraker accepts an HTTP request as authorized in this order (the first one that
applies wins):

1. **JWT**: `Authorization: Bearer <token>` — what browsers get after they log in
   with username + password in Mainsail/Fluidd.
2. **API key**: `X-Api-Key: <key>` — requests from non-browser clients.
3. **Trusted client**: the client IP or domain is listed under
   `[authorization] trusted_clients` (loopback and, on stock MainsailOS/KIAUH
   images, broad private-range entries).
4. Otherwise: **`401 Unauthorized`**.

When in doubt about *how a given request is being seen*, ask Moonraker directly:

```bash
curl -s http://<printer>:7125/access/info | jq
# {"default_source":"moonraker","available_sources":["moonraker"],"login_required":false,"trusted":true}
```

`login_required` reflects `force_logins`, `trusted` says whether this particular
curl connection is trusted.

### What this MCP server can and cannot use

- ✅ **Static API key** via `MOONRAKER_API_KEY` (sent as `X-Api-Key` on every
  request). This is *the* supported login.
- ❌ **Moonraker username + password** (the Mainsail/Fluidd login): those produce
  short-lived JWTs that must be refreshed — this server does not implement that
  flow, so it cannot replace your browser login.
- ❌ **Oneshot tokens** (`/access/oneshot_token`): they expire in 5 seconds and are
  meant for in-browser attachment hacks. Not a client login mechanism here.
- 🚫 **`force_logins: true`** in `moonraker.conf`: once users exist, *all*
  non-interactive clients (including this one and KlipperScreen-style apps) get
  rejected unless they do the interactive refresh flow. Keep it `false` and rely
  on the API key / `trusted_clients`.

### Get or rotate the API key

The key lives in Moonraker's database and is shown by a protected endpoint — query
it **from the printer itself** (SSH session), where the loopback connection is
trusted:

```bash
# read the current key
curl -s http://127.0.0.1:7125/access/api_key
# {"result": "e514851f37b94c779d955212b6906f95"}

# rotate (old key stops working immediately; open websockets stay valid until closed)
curl -s -X POST http://127.0.0.1:7125/access/api_key
```

Take the value from `result` (not the JSON wrapper) into:

```bash
export MOONRAKER_API_KEY="e514851f37b94c779d955212b6906f95"
mcp-moonraker doctor    # should now pass the Moonraker check
```

### Recommended `moonraker.conf` shapes

```ini
# TIGHTENED LAN (recommended): loopback + this MCP's host stay trusted for plugins,
# everybody else must present the API key.
[authorization]
trusted_clients:
    127.0.0.1                          # keep plugins on the printer itself working
    192.168.1.10/32                    # the host running OpenCode/mcp-moonraker, optional

# STRICT: no trusted clients (loopback aside); every non-browsing client must send
# the API key. Works fine for this MCP as long as MOONRAKER_API_KEY is set.
[authorization]
trusted_clients:
    127.0.0.1
```

Checkout the ordering rules:

- `cors_domains` is **irrelevant to this MCP**. CORS is a browser concept; the MCP
  server is not a browser. Don't add any entries *for the MCP*.
- Keep `127.0.0.1` in `trusted_clients` even on a locked-down install: helper
  services running on the printer itself (moonraker-obico, OctoEverywhere,
  KlipperScreen, timelapse) often talk to Moonraker over loopback and lose access
  the moment you drop it.
- Note: an IP that *is* trusted still gets a request rejected if it sends a
  **wrong** `X-Api-Key` header — if the MCP works with the header unset but not
  with one, the variable holds a stale value. Remove it or update it.
- After editing `moonraker.conf`, restart Moonraker:
  `sudo systemctl restart moonraker` (or Power → Restart Moonraker in the UI).

## Step 4 — Wire it into OpenCode

OpenCode launches the process with exactly the environment you configure, plus a
minimal inherited one. Put everything in the registration; **use an absolute
binary path** so `PATH` inheritance cannot hide it:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "moonraker": {
      "type": "local",
      "command": ["/home/you/.cargo/bin/mcp-moonraker"],
      "enabled": true,
      "environment": {
        "MOONRAKER_URL": "http://192.168.1.50:7125",
        "MOONRAKER_API_KEY": "...",
        "MCP_MOONRAKER_APPROVAL_MODE": "in_band"
      }
    }
  }
}
```

If the binary also needs a config file, pass it and point to it explicitly:

```json
"command": ["/home/you/.cargo/bin/mcp-moonraker", "--config", "/home/you/mcp-moonraker.toml"]
```

Two verification commands prove what the *server process* is actually seeing
(secrets are omitted from output by design):

```bash
mcp-moonraker print-config          # resolved URL, flags, paths, drivers
mcp-moonraker doctor                # probes Moonraker with those same values
```

Rules of thumb:

- The JSON block above is the source of truth for the env. `export FOO=...` in
  your own terminal does **not** affect the process OpenCode spawns.
- If you use the TOML file, `MOONRAKER_URL` there still beats a stale env var in
  case you pass `--config`. Environment beats the file, CLI beats both, so double
  check `print-config` output when editing both.

## Step 5 — Verify end to end

Fastest way to prove the whole path without launching an agent:

```bash
python3 scripts/smoke_mcp.py "$(which mcp-moonraker)"
```

or directly emulate the exact requests the MCP sends:

```bash
# reachability + HTTP status
curl -v http://192.168.1.50:7125/server/info

# same JSON-RPC request mcp-moonraker makes — without and with the key
curl -s -X POST http://192.168.1.50:7125/server/jsonrpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"server.info","id":1}'
# → {"error":{"code":401,"message":"Unauthorized",...}} means Moonraker wants auth

curl -s -X POST http://192.168.1.50:7125/server/jsonrpc \
  -H 'Content-Type: application/json' \
  -H "X-Api-Key: $MOONRAKER_API_KEY" \
  -d '{"jsonrpc":"2.0","method":"server.info","id":2}'
# → {"result":{...}} means the key is accepted

# what the MCP's own transport sees (stderr only; stdout carries MCP frames)
MCP_MOONRAKER_LOG_LEVEL=mcp_moonraker=debug \
  MOONRAKER_URL=http://192.168.1.50:7125 \
  mcp-moonraker doctor
```

If the first curl works and the JSON-RPC POST with a key returns `{"result":...}`,
your printer is fine — anything left wrong is in [Step 4](#step-4--wire-it-into-opencode).

## Diagnostics checklist

Work top-to-bottom; the first line that behaves differently marks the faulty hop.

0. **From OpenCode, call `moonraker_verify_connection` first.** It is the only
   tool that never hard-fails: it returns `connected`, latency, versions, and —
   when the endpoint cannot be reached or rejects credentials — a classified
   diagnosis (`connection refused`, DNS, timeout, TLS...) plus concrete next
   steps in one call. Use it to stop an agent from chaining further calls into
   the same failure.
1. **Is Moonraker even running, on the host/port you think?**
   On the printer: `systemctl status moonraker` — and note the `moonraker.log`
   line `Starting Moonraker on (0.0.0.0, 7125)`; that is the truth, not your
   memory or a random tutorial. `moonraker.conf` `[server]` `host:`/`port:`
   override it.
2. **Can the machine running mcp-moonraker reach it?** Run the curls from
   [Step 5](#step-5--verify-end-to-end) *on that machine*, not on the printer.
   From a container, `127.0.0.1` is the container itself.
3. **Is the name resolving?** `MOONRAKER_URL` with a `.local` hostname requires
   mDNS on the MCP host. `ping printer.local` failing there means use the IP.
4. **What does Moonraker say?** `curl http://<printer>:7125/access/info`
   (`trusted` = this connection is trusted, `login_required` = `force_logins` is
   active and blocks non-login clients).
5. **Does the exact JSON-RPC request succeed with the key?** The
   `X-Api-Key` curl in Step 5 is a byte-exact stand-in for the MCP server.
6. **Does the server itself agree with your head?** `mcp-moonraker print-config`
   shows the resolved URL and whether an API key was provided — compare to what
   you *think* you configured.
7. **Are you testing through OpenCode or standalone?** `doctor`, `print-config`,
   and the curls all run from your shell. If those pass but OpenCode fails, the
   difference is the `environment` block in `opencode.json`.

### What the error messages mean

```
Moonraker transport error: ...

  + hint appended automatically by this server, based on the failure class:

  connection refused    → nothing is listening at that host:port — wrong URL,
                          Moonraker not running, or 127.0.0.1 inside a container
  name resolution       → hostname could not be resolved — use the IP, or fix
                          mDNS/DNS on the host running mcp-moonraker
  network unreachable / → no route or firewall drop — routing/VPN/firewall to
  contact timeout         the printer's port
  request timeout       → Moonraker is listening but not answering — overloaded
                          host, or a timeout set below Moonraker's real latency
  certificate           → TLS problem — self-signed/proxy cert; see nginx section

Moonraker API error 401: Unauthorized
  + hint appended: Moonraker rejected the request — missing/stale
    MOONRAKER_API_KEY, a stale rotated key, or force_logins.
```

## Troubleshooting matrix

| Symptom | Most likely cause | Fix |
| --- | --- | --- |
| Every tool says "Moonraker is unreachable or returned a transport error" | OpenCode's environment did not include `MOONRAKER_URL`, so the server fell back to `http://127.0.0.1:7125` while running somewhere else | Call `moonraker_verify_connection` and read its `diagnosis`/`next_steps`; put `MOONRAKER_URL` in `opencode.json` `environment` (or a TOML file passed with `--config`), restart OpenCode, confirm with `print-config` |
| Same message, URL is correct, `doctor` says "connection refused" | Moonraker not running, or Moonraker listens on another port, or firewall | `systemctl status moonraker`, check `moonraker.log` "Starting Moonraker on (...)", port-match, open firewall |
| `doctor` reports `Moonraker API error 401` but browser UI works fine | Browser is a *trusted client* or logged-in user; the MCP is neither | Get/rotate the API key (Step 3), set `MOONRAKER_API_KEY`; do not rely on `cors_domains` |
| 401 originally, then stays 401 after setting the key | Key value stale or wrong source (decoded `{"result": ...}` vs raw string), or key rotated elsewhere | Re-fetch with `curl http://127.0.0.1:7125/access/api_key`, paste the `result` value, restart OpenCode |
| Calls time out after 15 s repeatedly | Moonraker overloaded or the route is genuinely broken | First prove reachability with `curl`; only then raise `moonraker.timeout_secs` |
| Works in OpenCode but `doctor` inside the same shell agrees nothing is wrong | Environment mismatch, or approvals/policy — inspect `MCP_MOONRAKER_LOG_LEVEL=mcp_moonraker=debug` stderr output of the *spawned* process | Launch the binary as OpenCode does — with that exact `opencode.json` `environment` — and call `tools` manually |
| Tools appear in OpenCode but every call returns some other error | The Moonraker request itself is failing per-call — the hints in the error text tell you which of the failure classes above applies | Follow the diagnostic hint in the error message; see the "What the error messages mean" block |
| Camera tool fails while status tools work | A `localhost`/`127.0.0.1` `snapshot_url` from a *different* than the MCP's host, or camera host on another subnet | Fix the webcam's `snapshot_url` to a reachable host, or set `camera.allowed_hosts` |
| No tools listed at all in OpenCode | Stdout polluted (something the process wrote to stdout), the binary path is wrong, or OpenCode could not spawn it | Use absolute path; check `MCP_MOONRAKER_LOG_LEVEL=... mcp-moonraker` manually; `enabled: true` in `opencode.json` |
| Everything was fine, prints fine, then stopped | Moonraker updated and the API key rotated, or the printer got a new IP | Re-fetch the API key and re-check `print-config` |

## Security checklist for exposed printers

For a printer that is reachable from outside your LAN (or from an untrusted LAN):

- **Prefer a VPN (WireGuard/Tailscale) over any direct exposure.** Nothing to
  encrypt at the HTTP layer, nothing to leak in URLs.
- **Never plain-port-forward Moonraker's 7125 raw.** It's plain HTTP and the API
  key carries full privileges (config writes, host administration). Behind nginx
  at minimum add TLS and a second authentication layer.
- **Rotate the API key** if it ever appears in a screenshot, an error report, or a
  shell history: `curl -X POST http://127.0.0.1:7125/access/api_key`.
- On Moonraker, tighten `trusted_clients` — the stock MainsailOS/KIAUH list trusts
  *all* RFC1918 ranges, meaning any device on the LAN gets full access without a
  key unless you trim it (see the tightened example in Step 3).
- `force_logins: true` is not a substitute for trimming `trusted_clients`: it
  *breaks* non-interactive clients such as this MCP (and KlipperScreen) rather
  than hardening them.
- `cors_domains` only affects browsers; it adds nothing for the MCP.
- On this MCP server's side, prefer the stdio transport (no listening socket at
  all); for the HTTP transport, keep it loopback and let a reverse proxy provide
  TLS, or set a strong `MCP_MOONRAKER_HTTP_TOKEN`. See
  [docs/deployment.md](deployment.md).
- For read-only or no-touch setups set `policy.read_only = true`; per-tool
  capability flags (`allow_raw_gcode`, `allow_host_admin`) stay off by default.
- See [SECURITY.md](../SECURITY.md) for the threat model of this server itself.
