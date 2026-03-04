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
