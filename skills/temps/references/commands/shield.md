<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `shield` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`shield list-events`](#shield-list-events)
- [`shield lookup-client`](#shield-lookup-client)
- [`shield get-summary`](#shield-get-summary)
- [`shield list-tokens`](#shield-list-tokens)
- [`shield create-token`](#shield-create-token)
- [`shield revoke-token`](#shield-revoke-token)
- [`shield list-entries`](#shield-list-entries)
- [`shield create-entry`](#shield-create-entry)
- [`shield delete-entry`](#shield-delete-entry)
- [`shield update-entry`](#shield-update-entry)
- [`shield get-settings`](#shield-get-settings)
- [`shield update-settings`](#shield-update-settings)
- [`shield list-platform-entries`](#shield-list-platform-entries)
- [`shield create-platform-entry`](#shield-create-platform-entry)
- [`shield delete-platform-entry`](#shield-delete-platform-entry)
- [`shield update-platform-entry`](#shield-update-platform-entry)
- [`shield list-proposals`](#shield-list-proposals)
- [`shield approve-proposal`](#shield-approve-proposal)
- [`shield reject-proposal`](#shield-reject-proposal)

## `shield`

Shield: bot and abuse protection rules, entries and proposals

**Subcommands:**

- `list-events` - Recent Shield events for an environment
- `lookup-client` - Look up what Shield says about a client
- `get-summary` - Shield summary for an environment
- `list-tokens` - List bypass tokens for an environment
- `create-token` - Create a bypass token
- `revoke-token` - Revoke a bypass token
- `list-entries` - List a project's Shield entries
- `create-entry` - Add a Shield entry to a project
- `delete-entry` - Remove a project's Shield entry
- `update-entry` - Change a project's Shield entry
- `get-settings` - Get the platform Shield settings
- `update-settings` - Change the platform Shield settings
- `list-platform-entries` - List platform Shield entries
- `create-platform-entry` - Add a platform Shield entry
- `delete-platform-entry` - Remove a platform Shield entry
- `update-platform-entry` - Change a platform Shield entry
- `list-proposals` - List fleet learning's proposals
- `approve-proposal` - Approve a proposal
- `reject-proposal` - Reject a proposal

### `shield list-events`

Recent Shield events for an environment

**Usage:** `shield list-events <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--limit <n>` | Events to return, 1 to 100; 20 by default | - | No |
| `--kind <value>` | Only this kind: 'verdict', 'challenge_issued', 'challenge_solved' or 'challenge_failed' | - | No |
| `--verdict <value>` | Only this verdict: 'block', 'challenge' or 'log' | - | No |
| `--ip <value>` | Only this client address, IPv4 or IPv6 | - | No |
| `--before <value>` | Only events before this instant (the 'next_before' of the previous page) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield lookup-client`

Look up what Shield says about a client

**Usage:** `shield lookup-client <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ip <value>` | A client address | - | No |
| `--ja4 <value>` | A JA4 TLS fingerprint | - | No |
| `--json` | Output in JSON format | - | No |

### `shield get-summary`

Shield summary for an environment

**Usage:** `shield get-summary <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--window <value>` | '1h', '24h' (the default) or '7d' (one of: 1h, 24h, 7d) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-tokens`

List bypass tokens for an environment

**Usage:** `shield list-tokens <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield create-token`

Create a bypass token

**Usage:** `shield create-token <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--name <value>` | What uses it: 'uptime monitor', 'CI' [required] | - | No |
| `--project-wide` | Valid for every environment of the project, not only this one | - | No |
| `--no-project-wide` | Set project_wide to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield revoke-token`

Revoke a bypass token

**Usage:** `shield revoke-token <environment-id> <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-entries`

List a project's Shield entries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield create-entry`

Add a Shield entry to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expires-at <value>` | When the entry stops applying; omit for never | - | No |
| `--ja4 <value>` | A JA4 fingerprint; exactly one of 'network' and 'ja4' | - | No |
| `--network <value>` | An address or CIDR range; exactly one of 'network' and 'ja4' | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield delete-entry`

Remove a project's Shield entry

**Usage:** `shield delete-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `shield update-entry`

Change a project's Shield entry

**Usage:** `shield update-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expires-at <value>` | Body field expires_at | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield get-settings`

Get the platform Shield settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield update-settings`

Change the platform Shield settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--share-observations` | Whether what one environment's visitors did counts for every environment (D9): addresses and fingerprints... | - | No |
| `--no-share-observations` | Set share_observations to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-platform-entries`

List platform Shield entries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield create-platform-entry`

Add a platform Shield entry

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expires-at <value>` | When the entry stops applying; omit for never | - | No |
| `--ja4 <value>` | A JA4 fingerprint; exactly one of 'network' and 'ja4' | - | No |
| `--network <value>` | An address or CIDR range; exactly one of 'network' and 'ja4' | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield delete-platform-entry`

Remove a platform Shield entry

**Usage:** `shield delete-platform-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `shield update-platform-entry`

Change a platform Shield entry

**Usage:** `shield update-platform-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expires-at <value>` | Body field expires_at | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-proposals`

List fleet learning's proposals

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Only proposals in this state; all of them when omitted (one of: pending, approved, rejected) | - | No |
| `--q <value>` | Only proposals whose address or fingerprint contains this text, ignoring case | - | No |
| `--page <n>` | Page number, from 1 (the default) | - | No |
| `--page-size <n>` | Proposals per page: 25 by default, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

### `shield approve-proposal`

Approve a proposal

**Usage:** `shield approve-proposal <proposal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield reject-proposal`

Reject a proposal

**Usage:** `shield reject-proposal <proposal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
