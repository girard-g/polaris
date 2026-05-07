# Migration Guide: Single-Tenant → Multi-Tenant

> **Status: forward-looking spec.** This document describes a planned multi-tenant deployment mode and the `polaris migrate` / `polaris backup` tooling that would support it. Neither the multi-tenant runtime nor the migration command exists in the current codebase. The spec is kept here as design context for future work; nothing in it should be read as describing currently-shipping behaviour.

This guide covers upgrading a single-tenant Polaris deployment to multi-tenant mode.

---

## Overview

In single-tenant mode, all documents are stored in a single `polaris.db` file. In multi-tenant mode, documents are stored in per-namespace `.db` files under `data_dir`. The `polaris migrate` command copies a single-tenant database into a multi-tenant namespace DB, leaving the original untouched.

---

## Prerequisites

- A Polaris build with multi-tenant support installed
- **The single-tenant server must be stopped before running `polaris migrate`** — the live server holds an open WAL writer lock on the database, and migrating against a running server risks reading inconsistent state
- The single-tenant `polaris.db` file accessible on disk
- A configured `data_dir` in `polaris.toml` (or rely on the XDG default — see Step 5)

---

## Step-by-Step Migration

### 1. Back up your existing database

```bash
cp polaris.db polaris.db.backup
```

Never skip this step. The migration does not modify the source DB, but having a backup ensures you can return to single-tenant mode at any point.

> Once you are on multi-tenant, use `polaris backup --dest <path>` to back up all namespace DBs and `polaris-mgmt.db` atomically (safe while the server is running). See [`polaris backup`](cli.md#polaris-backup) for details.

### 2. Decide on a target namespace

Your single-tenant data will become one namespace in multi-tenant mode. Typical choices:

| If you were using the single DB as... | Suggested namespace |
|--------------------------------------|---------------------|
| Shared team docs | `shared` |
| Personal project docs | `user/<your-cn>` |
| A single group's docs | `group/<group-name>` |

### 3. Run the migration

```bash
polaris migrate --from polaris.db --to-namespace shared --data-dir /var/lib/polaris
```

Output:

```
Opening source: polaris.db
Creating target namespace: shared → /var/lib/polaris/shared.db
Copying 47 documents, 523 chunks
Embedding model matches — copying vec_chunks directly
Rebuilding chunks_fts...
Done. Migrated 47 documents, 523 chunks to namespace shared.
Source database not modified. Remove it manually when ready.
```

### 4. Verify the migrated data

```bash
polaris status --namespace shared --data-dir /var/lib/polaris
```

Confirm document and chunk counts match the single-tenant source.

### 5. Switch the server config to multi-tenant

**Path A — Multi-tenant server deployment:**

Update `polaris.toml` to enable multi-tenant mode:

```toml
[multi_tenant]
enabled  = true
data_dir = "/var/lib/polaris"   # or omit to use the XDG default
```

See [multi-tenant.md](multi-tenant.md) for the full configuration reference. Then restart the server.

**Path B — Local single-tenant users (`polaris serve` stdio):**

No config change needed. After running `polaris migrate`, `polaris serve` auto-discovers all `.db` files under `data_dir` and fans out search across all of them. Your existing MCP config entry (`polaris serve`) continues to work unchanged.

If `--data-dir` was omitted from `polaris migrate`, it defaults to the XDG data directory per platform:

| OS | Default `data_dir` |
|----|-------------------|
| Linux | `~/.local/share/polaris/` |
| macOS | `~/Library/Application Support/polaris/` |
| Windows | `%APPDATA%\polaris\` |

### 6. Remove the old database (optional)

Once you've verified the multi-tenant deployment is working correctly, you can remove the old `polaris.db`:

```bash
rm polaris.db
```

The migration command intentionally leaves it in place — deletion is a manual step so you retain a recovery path during the transition period.

---

## Re-embedding After Model Change

If you upgrade to a different embedding model during the migration, the migrated chunks must be re-embedded (the stored vectors are incompatible):

```bash
polaris migrate --from polaris.db --to-namespace shared --data-dir /var/lib/polaris
# polaris detects model_id or embedding_dim mismatch and re-embeds automatically
```

Re-embedding is CPU/GPU intensive and proportional to chunk count. Progress is printed to stdout. The source DB is not modified.

---

## Migrating Multiple Databases

If you were using `polaris search --db db1.db --db db2.db` in single-tenant mode, migrate each DB into its own namespace:

```bash
polaris migrate --from ~/docs/library.db --to-namespace group/library --data-dir /var/lib/polaris
polaris migrate --from ~/docs/internal.db --to-namespace group/internal --data-dir /var/lib/polaris
```

---

## Failure Handling

- **Target namespace already exists:** Migration fails with `"namespace already exists; use --force to overwrite"`. Use `--force` to replace an existing namespace DB.
- **Unknown source schema version:** Fails with `"unsupported source schema version N"` if the source DB is newer than the migrator understands.
- **Partial failure mid-migration:** The target DB is written to `<namespace>.db.tmp` and renamed to `<namespace>.db` only on full success. If any step fails, `.db.tmp` is removed, leaving no corrupt partial state.

---

## Rollback to Single-Tenant

Multi-tenant mode cannot automatically downgrade to single-tenant. To rollback:

1. Stop the multi-tenant server
2. Restore the backup: `cp polaris.db.backup polaris.db`
3. Reinstall the single-tenant binary
4. Point the config at the original `polaris.db`
5. Start the single-tenant server

The `polaris-mgmt.db` and namespace DBs created by multi-tenant mode can be left in place — they are ignored by single-tenant binaries. Remove them manually when you're confident the rollback is permanent.

---

## Namespace DB Schema Note

Namespace DBs in multi-tenant mode share the same schema as the single-tenant `polaris.db` of the same Polaris release — whatever `SCHEMA_VERSION` the build ships with. There are no multi-tenant-specific columns or tables in the namespace DBs themselves; multi-tenancy is implemented at the file/process layer, not in the schema. Schema migrations are applied identically across all namespace DBs by `Database::open()`. See [database.md](database.md) for the full schema reference.
