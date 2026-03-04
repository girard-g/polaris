# Polaris Multi-Tenant Deployment (v3 Planned)

> **Status:** Planned for v3. Not yet implemented. This document describes the intended design.
>
> For standard single-user deployments, none of this applies — the default mode remains a single local binary with no networking.

---

## When to Use This

Multi-tenancy is **only relevant for company or team server deployments** where multiple people share one Polaris instance over a network. Examples:

- Engineering team hosting shared library/framework docs all members search
- Company knowledge base served to all employees' coding agents
- Group-isolated docs (e.g., frontend vs. backend teams have separate private namespaces)

For personal/per-project use, run the standard `polaris serve` (stdio MCP). No TLS, no namespaces, no config needed.

---

## First Deployment Walkthrough

This walkthrough takes a sysadmin from zero to a running multi-tenant Polaris instance. Steps 1–5 are done by the admin on the server. Steps 6–8 are repeated for each new user.

### Cross-platform notes

The **server** is typically deployed on Linux. The **client** (`polaris connect --setup`, `polaris cert export`) works on Linux, macOS, and Windows.

**Config directories used by `polaris connect --setup`:**

| OS | Default config dir |
|----|--------------------|
| Linux | `~/.config/polaris/` |
| macOS | `~/Library/Application Support/polaris/` |
| Windows | `%APPDATA%\polaris\` |

The printed Claude Code snippet always uses the correct absolute path for the current OS.

**openssl on macOS:** The system `openssl` is actually LibreSSL with different flag behavior. Use `brew install openssl` and reference it explicitly (`$(brew --prefix openssl)/bin/openssl`) for server-side CA/cert generation.

**openssl on Windows:** Use Git for Windows (includes openssl), or install via `winget install ShiningLight.OpenSSL`. Commands are the same but paths use backslashes.

---

### Step 1 — Create system user and directories (Linux)

```bash
sudo useradd -r -s /bin/false -d /var/lib/polaris polaris
sudo mkdir -p /var/lib/polaris/{group,user}
sudo mkdir -p /etc/polaris
sudo chown -R polaris:polaris /var/lib/polaris /etc/polaris
sudo chmod 700 /etc/polaris
```

For macOS (launchd) or Windows (Windows Service), the data dir and config dir are set in `polaris.toml` — no system user is strictly required for small teams.

---

### Step 2 — Generate PKI (CA + server cert only)

```bash
cd /etc/polaris

# Internal CA (10 years)
sudo openssl req -x509 -newkey rsa:4096 -days 3650 -nodes \
  -keyout ca.key -out ca.crt \
  -subj "/CN=Polaris Internal CA/O=My Company"

# Server cert
sudo openssl req -newkey rsa:2048 -nodes -keyout server.key \
  -subj "/CN=polaris.company.internal" -out server.csr
sudo openssl x509 -req -days 365 -CA ca.crt -CAkey ca.key \
  -CAcreateserial -in server.csr -out server.crt
```

> **Note:** Client certs are never generated manually. Use `polaris user invite` instead (see [Automated User Enrollment](#automated-user-enrollment)).

---

### Step 3 — Bootstrap the first admin cert (one-time, for the sysadmin)

Because the server isn't running yet, the first admin cert must be generated manually:

```bash
cat > /tmp/admin-ext.cnf <<'EOF'
[SAN]
subjectAltName=DNS:group.admin
EOF

openssl req -newkey rsa:2048 -nodes -keyout ~/admin.key \
  -subj "/CN=admin" -out admin.csr
openssl x509 -req -days 365 -CA /etc/polaris/ca.crt -CAkey /etc/polaris/ca.key \
  -CAcreateserial -in admin.csr -out ~/admin.crt \
  -extfile /tmp/admin-ext.cnf -extensions SAN

# Copy to local polaris config dir (adjust path per OS)
mkdir -p ~/.config/polaris          # Linux
cp ~/admin.crt ~/.config/polaris/client.crt
cp ~/admin.key ~/.config/polaris/client.key
cp /etc/polaris/ca.crt ~/.config/polaris/ca.crt
```

This is the **only time** openssl is used for a client cert. All subsequent user certs use the automated enrollment flow.

---

### Step 4 — Write `polaris.toml` and `namespaces.toml`

`/etc/polaris/polaris.toml`:

```toml
[tls]
cert = "/etc/polaris/server.crt"
key  = "/etc/polaris/server.key"
ca   = "/etc/polaris/ca.crt"

[multi_tenant]
enabled           = true
namespaces_config = "/etc/polaris/namespaces.toml"
data_dir          = "/var/lib/polaris"

[web_ui]
enabled = true

# [observability]
# metrics      = false   # expose /metrics endpoint
# metrics_auth = false   # require client cert for /metrics
# log_format   = "text"  # "text" or "json"
```

`/etc/polaris/namespaces.toml`:

```toml
[namespace."shared"]
writers = ["group.admin"]

[namespace."user.*"]
writers = ["self"]
```

---

### Step 5 — Create initial namespaces and start the server

**Linux (systemd):**

```bash
polaris namespace create shared
sudo systemctl enable --now polaris
```

**macOS (launchd):**

```bash
polaris namespace create shared
sudo cp polaris.plist /Library/LaunchDaemons/com.polaris.server.plist
sudo launchctl load /Library/LaunchDaemons/com.polaris.server.plist
```

**Windows (PowerShell, as admin):**

```powershell
polaris.exe namespace create shared
New-Service -Name "Polaris" `
  -BinaryPathName "C:\polaris\polaris.exe serve-https --config C:\polaris\polaris.toml" `
  -StartupType Automatic
Start-Service Polaris
```

**Docker:**

```bash
polaris namespace create shared   # run once to init the data volume

docker compose up -d
```

**Smoke test (all platforms):**

```bash
curl --cert ~/.config/polaris/client.crt --key ~/.config/polaris/client.key \
     --cacert ~/.config/polaris/ca.crt \
     https://polaris.company.internal:8443/health
# → {"status":"ok"}
```

---

### Step 6 — Admin invites a user

```bash
polaris user invite alice --groups frontend,backend
```

Output:

```
Invite token for alice (groups: frontend, backend):

  Token:   a3f8c2d1...
  Expires: 2026-03-05 10:00 UTC
  Command: polaris connect --setup https://polaris.company.internal:8443 --token a3f8c2d1...

Share the token with alice. It can be used once and expires in 24h.
```

Copy the printed command and send it to alice (Slack, email, etc.).

---

### Step 7 — User sets up their client (Linux / macOS / Windows)

Alice runs on her own machine:

```bash
polaris connect --setup https://polaris.company.internal:8443 --token a3f8c2d1...
```

Polaris generates a key locally, sends a CSR to the server, receives and saves the signed cert, then prints:

```
Setup complete. Add this to your Claude Code config:

{
  "mcpServers": {
    "polaris": {
      "command": "polaris",
      "args": ["connect", "https://polaris.company.internal:8443"],
      "env": {
        "POLARIS_CLIENT_CERT": "/home/alice/.config/polaris/client.crt",
        "POLARIS_CLIENT_KEY":  "/home/alice/.config/polaris/client.key",
        "POLARIS_CA_CERT":     "/home/alice/.config/polaris/ca.crt"
      }
    }
  }
}
```

Alice copies the JSON snippet into her Claude Code config. Done.

---

### Step 8 — User accesses the Web UI (optional)

Export the cert as PKCS#12 (no openssl needed):

```bash
polaris cert export --format p12 --out polaris.p12
```

Import in browser:

- **Firefox** (all platforms): Settings → Privacy → Certificates → Your Certificates → Import
- **Chrome/Edge on macOS**: Double-click `polaris.p12` → Keychain Access imports it automatically
- **Chrome/Edge on Windows**: Double-click `polaris.p12` → Certificate Import Wizard
- **Chrome/Edge on Linux**: Settings → Privacy → Manage Certificates → Your Certificates → Import

Navigate to `https://polaris.company.internal:8443/ui/`. The browser will prompt to select the client cert.

---

## Namespace Model

Documents are stored in isolated SQLite databases, one per namespace. Three tiers:

```
shared/               → readable by everyone
group/{name}/         → readable by members of group {name}
user/{cn}/            → readable only by the user with CN={cn}
```

Each namespace maps to a separate `.db` file on disk:

```
~/.local/share/polaris/
  shared.db
  group/frontend.db
  group/backend.db
  user/alice.db
  user/bob.db
```

This gives hard isolation: a compromised or corrupt namespace cannot affect others. No cross-namespace data leaks at the storage layer.

### Namespace Resolution at Search Time

When a user searches, Polaris fans out the query to all namespaces they can access:
1. `shared/`
2. All `group/{name}/` where the user belongs to `{name}`
3. Their own `user/{cn}/`

Results from each namespace are collected and globally re-ranked using Reciprocal Rank Fusion (RRF), then deduplicated and trimmed to the requested `top_k`.

---

## Authentication: mTLS

Multi-tenant mode requires mutual TLS. Both the server and the client present certificates.

### Certificate Design

| Cert field | Meaning |
|------------|---------|
| Server CN  | Hostname (e.g. `polaris.company.internal`) |
| Client CN  | Username (e.g. `alice`) |
| Client SAN | `dns:group.frontend`, `dns:group.backend`, ... |

Group membership is encoded directly in the client certificate's SAN (Subject Alternative Name) fields using the `dns:group.{name}` convention. No external directory lookup needed.

### CA Setup (example)

```bash
# 1. Create a local CA
openssl req -x509 -newkey rsa:4096 -days 3650 -nodes \
  -keyout ca.key -out ca.crt \
  -subj "/CN=Polaris Internal CA"

# 2. Issue a server cert
openssl req -newkey rsa:2048 -nodes -keyout server.key \
  -subj "/CN=polaris.company.internal" -out server.csr
openssl x509 -req -days 365 -CA ca.crt -CAkey ca.key \
  -CAcreateserial -in server.csr -out server.crt

# 3. Issue a client cert for alice (frontend + backend groups)
cat > alice-ext.cnf <<EOF
[SAN]
subjectAltName=DNS:group.frontend,DNS:group.backend
EOF
openssl req -newkey rsa:2048 -nodes -keyout alice.key \
  -subj "/CN=alice" -out alice.csr
openssl x509 -req -days 365 -CA ca.crt -CAkey ca.key \
  -CAcreateserial -in alice.csr -out alice.crt \
  -extfile alice-ext.cnf -extensions SAN
```

The server validates client certs against the CA at the TLS handshake. Requests with no valid client cert are rejected before any application logic runs.

---

## Automated User Enrollment

The manual openssl flow (generate key → generate CSR → sign cert → distribute files) is error-prone for end users. Polaris v3 provides a two-command enrollment flow that keeps the private key on the user's machine.

### Flow overview

```
Admin                                    Server                     User (Alice)
  │                                         │                           │
  │  polaris user invite alice              │                           │
  │  --groups frontend,backend              │                           │
  ├────────────────────────────────────────►│                           │
  │◄── one-time token (expires in 24h) ─────┤                           │
  │                                         │                           │
  │  (shares token with alice out-of-band)  │                           │
  │                                         │                           │
  │                                         │  polaris connect --setup  │
  │                                         │  https://server:8443      │
  │                                         │  --token <token>          │
  │                                         │◄──────────────────────────┤
  │                                         │  (alice's CSR posted)     │
  │                                         ├──► signed cert + CA cert ─►│
  │                                         │                           │
  │                                         │   saves to ~/.config/polaris/
  │                                         │   prints Claude Code snippet
```

**Key security property:** Alice's private key is generated locally by `polaris connect --setup`. Only the CSR (public key + CN + SANs) is sent to the server. The private key never leaves Alice's machine.

---

### `polaris user invite <cn> --groups <g1,g2> [--expires 24h]`

Creates a pending enrollment entry on the server and prints a one-time token:

```
Invite token for alice (groups: frontend, backend):

  Token:   a3f8c2d1...
  Expires: 2026-03-05 10:00 UTC
  Command: polaris connect --setup https://polaris.company.internal:8443 --token a3f8c2d1...

Share the token with alice. It can be used once and expires in 24h.
```

The admin can also generate tokens from the Web UI: `/ui/admin/` → Users → "Invite user" button, which displays the token and copy-paste command inline.

---

### `polaris connect --setup <url> --token <token>`

Run by the user on their own machine:

1. Generates a 2048-bit RSA key locally (saved to the OS-appropriate config dir)
2. Posts a CSR to `/enroll/<token>` on the server
3. Server validates the token, signs the CSR with the CA, returns cert + CA cert
4. Saves `client.crt` and `ca.crt` to the config dir
5. Marks the token as used (one-time, cannot be reused)
6. Prints the Claude Code config snippet to stdout

---

### `polaris connect <url>`

Used for subsequent connections after setup. Reads certs from the config dir and connects to the MCP-over-HTTPS server. This replaces the stdio `polaris serve` for multi-tenant deployments.

---

### `polaris cert export --format p12 --out <file>`

Packages the client cert + key + CA cert into a PKCS#12 file for browser import. Prompts for a passphrase. No openssl knowledge required.

---

## User Lifecycle Management

Certs expire and team members leave. This section covers how admins keep the user roster up to date without touching openssl.

### Management DB

Polaris maintains a `users` table in a management database at `<data_dir>/polaris-mgmt.db`:

| Column | Type | Notes |
|--------|------|-------|
| cn | TEXT | username (PK) |
| groups | TEXT | comma-separated group names |
| serial | INTEGER | cert serial number |
| issued_at | TEXT | ISO 8601 |
| expires_at | TEXT | ISO 8601 |
| revoked | BOOLEAN | default false |

The row is created when a `polaris user invite` token is consumed during `polaris connect --setup`. Revocation is checked per-request against this table (in addition to the TLS handshake), so blocking a user does not require CA private-key access or CRL generation.

---

### `polaris user list [--expiring <duration>]`

```
CN       Groups              Issued      Expires     Status
admin    admin               2026-01-01  2027-01-01  ok
alice    frontend,backend    2026-03-01  2027-03-01  ok (364d)
bob      backend             2026-02-01  2026-04-01  EXPIRING (28d)
carol    frontend            2025-12-01  2026-02-01  EXPIRED
```

`--expiring 30d` filters to certs expiring within 30 days — useful in cron jobs or alerting scripts.

---

### `polaris user revoke <cn>`

1. Marks the user as revoked in the management DB
2. Subsequent requests from that CN are rejected with HTTP 403, even if the TLS handshake succeeds (i.e., the cert is still technically valid)
3. Optionally deletes the `user/<cn>` namespace (flag: `--delete-namespace`)

Output:

```
Revoked alice. Certificate serial 42 is now blocked.
Use 'polaris user renew alice' to issue a new cert if needed.
```

---

### `polaris user renew <cn>`

Generates a new enrollment token for an existing user, pre-filling CN and groups from the management DB:

```
Renewal token for alice (groups: frontend, backend):

  Token:   b7d2e9f3...
  Expires: 2026-03-05 10:00 UTC
  Command: polaris connect --setup https://polaris.company.internal:8443 --token b7d2e9f3...

Old cert remains valid until expiry unless you also revoke it:
  polaris user revoke alice --before-renew
```

Flag `--revoke-old` revokes the current cert immediately when the renewal token is issued.

---

### Web UI addition

The Admin UI "Users & Certs" page gains:
- **Renew** button — equivalent to `polaris user renew`, prints the enrollment command inline
- **Cert expiry column** — color-coded: green (> 30 days), amber (≤ 30 days), red (expired or revoked)

---

## Permission Config: `namespaces.toml`

Fine-grained read/write control is defined in `namespaces.toml`, which Polaris watches and hot-reloads (no restart needed).

```toml
# Who can index (write) into each namespace.
# Read access is derived from the namespace tier + mTLS groups.

[namespace."shared"]
writers = ["group.admin"]          # only admins may index shared docs

[namespace."group.frontend"]
writers = ["group.frontend"]       # frontend members index their own ns

[namespace."group.backend"]
writers = ["group.backend"]

[namespace."user.*"]
writers = ["self"]                 # each user owns their own namespace
```

**Rules:**

- `"self"` means the authenticated CN matches the namespace user component.
- `"group.{name}"` means the client cert SAN includes `DNS:group.{name}`.
- Read access for `shared/` is implicit for all authenticated users.
- Read access for `group/{name}/` requires the matching SAN.
- Read access for `user/{cn}/` requires `CN=cn`.
- An absent namespace entry defaults to: writers = [] (read-only from the tier's default grant).

---

## Configuration

Two new sections in `polaris.toml`:

```toml
[tls]
# Paths to PEM files for the HTTPS server
cert = "/etc/polaris/server.crt"
key  = "/etc/polaris/server.key"
ca   = "/etc/polaris/ca.crt"       # used to verify client certs

[multi_tenant]
enabled          = true
namespaces_config = "/etc/polaris/namespaces.toml"
data_dir         = "/var/lib/polaris"   # root for per-namespace .db files
```

Both sections are **opt-in**. When `[multi_tenant] enabled = false` (the default), Polaris behaves exactly as today: single SQLite file, no TLS, no namespace logic.

---

## New CLI Subcommands

### `polaris namespace create <namespace>`

Creates the SQLite database file for a new namespace.

```bash
polaris namespace create shared
polaris namespace create group/frontend
polaris namespace create user/alice
```

### `polaris namespace list`

Lists all known namespaces and their database sizes.

```
shared              2.1 MB
group/frontend      840 KB
group/backend       1.2 MB
user/alice          340 KB
```

### `polaris namespace delete <namespace>`

Deletes the namespace database after confirmation.

```bash
polaris namespace delete user/alice
# → "Delete namespace 'user/alice' and all its data? [y/N]"
```

### `polaris serve-https`

Starts the MCP-over-HTTPS server (as opposed to stdio). Reads `[tls]` and `[multi_tenant]` from config.

```bash
polaris serve-https --config /etc/polaris/polaris.toml
```

The transport is HTTP/1.1 + JSON (same MCP protocol, different transport). Each request is authenticated via mTLS before dispatch.

---

## Modified MCP Tools

### `index` — gains optional `namespace` parameter

```json
{
  "name": "index",
  "parameters": {
    "path": "docs/",
    "namespace": "group/frontend"   // optional; defaults to user's own namespace
  }
}
```

The caller must have write permission for the target namespace (checked against `namespaces.toml`). Attempting to write to an unauthorized namespace returns an MCP error.

### `search` — fans out automatically

No parameter changes. When multi-tenancy is enabled, `search` automatically queries all namespaces the authenticated user can read, merges results via RRF, and returns the top-k.

Each result gains a `provenance` field:

```json
{
  "score": 0.87,
  "content": "...",
  "source_file": "docs/api.md",
  "provenance": "group/frontend"
}
```

This tells the caller which namespace the chunk came from. In single-tenant mode, `provenance` is omitted.

### `status` — namespace-aware

Returns per-namespace document/chunk counts when multi-tenancy is enabled.

---

## Remote Indexing

The MCP `index` tool works for interactive sessions, but CI/CD pipelines need a way to push docs without an MCP agent session.

### New HTTP endpoint: `POST /api/index`

```
/api/index      → Remote indexing (mTLS authenticated, write-permission checked)
```

Request: `multipart/form-data` with:
- `namespace` (text field) — target namespace
- `file` (file field, repeatable) — one field per file

Response: JSON

```json
{"added": 12, "modified": 3, "removed": 0, "skipped": 1}
```

Auth: same mTLS as the MCP endpoint. Write permission is checked against `namespaces.toml` before any files are processed.

---

### `polaris index --remote <url> --namespace <ns> <path>`

```bash
# Push ./docs to group/frontend on the remote server
polaris index --remote https://polaris.company.internal:8443 \
  --namespace group/frontend \
  ./docs
```

1. Reads files from `<path>` locally (same discovery logic as local `polaris index`)
2. POSTs them to `POST /api/index` using the client mTLS cert from the config dir
3. Prints the server's response stats

Output:

```
Uploading docs/ → group/frontend on polaris.company.internal:8443
  47 files (2.3 MB)
Done: 47 added, 0 modified, 0 removed
```

This is the CI/CD-friendly path: a GitHub Actions step, Makefile target, or git hook can push docs to the appropriate namespace without an MCP agent session.

---

## Deployment Example

### Filesystem Layout

```
/etc/polaris/
  polaris.toml
  namespaces.toml
  server.crt
  server.key
  ca.crt

/var/lib/polaris/
  shared.db
  group/
    frontend.db
    backend.db
  user/
    alice.db
    bob.db
```

### systemd Unit

```ini
[Unit]
Description=Polaris MCP Server (multi-tenant)
After=network.target

[Service]
ExecStart=/usr/local/bin/polaris serve-https --config /etc/polaris/polaris.toml
Restart=on-failure
User=polaris
Group=polaris
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/polaris

[Install]
WantedBy=multi-user.target
```

### Dockerfile (multi-stage)

```dockerfile
FROM rust:1.83-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin polaris

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/polaris /usr/local/bin/polaris
EXPOSE 8443
ENTRYPOINT ["polaris", "serve-https", "--config", "/etc/polaris/polaris.toml"]
```

### docker-compose.yml

```yaml
services:
  polaris:
    image: polaris:latest
    ports:
      - "8443:8443"
    volumes:
      - /etc/polaris:/etc/polaris:ro    # certs + config (read-only mount)
      - polaris-data:/var/lib/polaris   # persistent data
    restart: unless-stopped

volumes:
  polaris-data:
```

**Notes:**
- The same PKI setup (Steps 1–3 of the walkthrough) applies unchanged — only Step 5 differs
- `polaris-data` volume survives container restarts and upgrades
- To upgrade: `docker pull polaris:latest && docker compose up -d`
- Pre-built images are published to GHCR: `ghcr.io/<org>/polaris:latest`

### Claude Code client config (per-user)

```json
{
  "mcpServers": {
    "polaris": {
      "command": "polaris",
      "args": ["connect", "https://polaris.company.internal:8443"],
      "env": {
        "POLARIS_CLIENT_CERT": "~/.config/polaris/alice.crt",
        "POLARIS_CLIENT_KEY":  "~/.config/polaris/alice.key",
        "POLARIS_CA_CERT":     "~/.config/polaris/ca.crt"
      }
    }
  }
}
```

---

## Web UI

Polaris v3 serves a browser-based admin and user interface on the **same port** as the MCP endpoint. Static assets are embedded in the binary at compile time via `rust-embed` — no separate web server, no separate deployment step.

### Routes

```
/mcp            → MCP-over-HTTPS (unchanged)
/ui/            → User dashboard (any authenticated user)
/ui/admin/      → Admin dashboard (SAN must include DNS:group.admin)
/enroll/<token> → One-time enrollment endpoint (unauthenticated, token-gated)
/health         → Health check (unauthenticated, returns JSON)
/metrics        → Prometheus metrics (unauthenticated by default, see Observability)
```

### Authentication

The same mTLS handshake that guards the MCP endpoint also gates the Web UI. The browser presents the client cert; Polaris reads the CN (username) and SAN (groups) from the validated cert on every request.

Requests to `/ui/admin/` that lack `DNS:group.admin` in the SAN receive HTTP 403.

For browser access, users export their client cert as PKCS#12 after running `polaris connect --setup` (see `polaris cert export` above) and import it in the browser once.

### Admin UI

| Page | Content |
|------|---------|
| Namespaces | List all namespaces + DB size; create / delete; visual `namespaces.toml` editor |
| Users & Certs | List active users by CN + groups; "Invite user" button (generates enrollment token); revoke (appends to CRL) |
| Monitoring | Per-namespace doc/chunk counts; embedding model info; recent request log |

### User UI

| Page | Content |
|------|---------|
| Search tester | Query box → results with scores, provenance, source file |
| My namespaces | List of accessible namespaces; file count; expandable file list |

### Config

```toml
[web_ui]
enabled     = true        # default true when [multi_tenant] enabled = true
path_prefix = ""          # set to "/polaris" for reverse-proxy deployments
```

---

## Observability

### Prometheus `/metrics` endpoint

When `metrics = true`, Polaris exposes a `/metrics` endpoint in Prometheus text exposition format:

| Metric | Type | Labels |
|--------|------|--------|
| `polaris_requests_total` | counter | namespace, method, status |
| `polaris_request_duration_seconds` | histogram | namespace, method |
| `polaris_namespace_documents_total` | gauge | namespace |
| `polaris_namespace_chunks_total` | gauge | namespace |
| `polaris_namespace_db_bytes` | gauge | namespace |
| `polaris_active_connections` | gauge | — |
| `polaris_embedding_duration_seconds` | histogram | — |

Default: unauthenticated (standard for internal Prometheus scraping behind a firewall). Set `metrics_auth = true` to require a valid client cert.

---

### JSON log format

With `log_format = "json"`, structured log lines are emitted as newline-delimited JSON:

```json
{"ts":"2026-03-04T10:00:00Z","level":"INFO","target":"polaris::auth","msg":"request authenticated","cn":"alice","groups":["frontend","backend"]}
{"ts":"2026-03-04T10:00:01Z","level":"INFO","target":"polaris::search","msg":"fan-out","ns":["shared","group/frontend"],"query":"..."}
```

Compatible with Loki, Elasticsearch, Splunk, and CloudWatch out of the box.

---

### Config

```toml
[observability]
metrics      = false        # expose /metrics endpoint (default false)
metrics_auth = false        # require client cert for /metrics
log_format   = "text"       # "text" (default) or "json"
```

---

## Security Notes

### Path Traversal Protection

Namespace names are validated against `[a-z0-9_-]+` at creation time. The `group/` and `user/` prefixes are the only allowed directory components. Any attempt to use `..`, absolute paths, or other components is rejected at the API boundary.

### Certificate Revocation

The initial implementation will support CRL (Certificate Revocation List) files, configured as:

```toml
[tls]
crl = "/etc/polaris/revoked.crl"   # optional
```

OCSP stapling is out of scope for v3 but may be added later.

### Namespace Isolation at the DB Layer

Each namespace is a completely separate SQLite file. There are no shared tables between namespaces. A query against `group/frontend.db` has no SQL-level access to `shared.db` — the fan-out is done in Rust code after both query results are returned, not via SQL JOIN.

### Audit Logging

When multi-tenancy is enabled, Polaris emits structured log lines for every authenticated request:

```
INFO polaris::auth: request authenticated cn=alice groups=[frontend,backend]
INFO polaris::search: fan-out ns=["shared","group/frontend","group/backend","user/alice"] query="..."
INFO polaris::index: write ns=group/frontend path=docs/components.md cn=alice
```

These are written to stderr and can be captured by journald or any log aggregator.
