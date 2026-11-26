<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `flags` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`flags list`](#flags-list)
- [`flags get`](#flags-get)
- [`flags create`](#flags-create)
- [`flags update`](#flags-update)
- [`flags rules`](#flags-rules)
- [`flags evaluate`](#flags-evaluate)
- [`flags reshuffle`](#flags-reshuffle)
- [`flags set`](#flags-set)
- [`flags clear`](#flags-clear)
- [`flags disable`](#flags-disable)
- [`flags enable`](#flags-enable)
- [`flags restore`](#flags-restore)
- [`flags archive`](#flags-archive)
- [`flags record-exposure`](#flags-record-exposure)
- [`flags get-snapshot`](#flags-get-snapshot)
- [`flags ofrep-server-bulk`](#flags-ofrep-server-bulk)
- [`flags ofrep-server-single`](#flags-ofrep-server-single)

## `flags` (alias: `flag`)

Manage feature flags (runtime config that changes without a redeploy)

**Subcommands:**

- `list` (`ls`) - List feature flags
- `get` - Show a feature flag and its per-environment values
- `create` - Create a feature flag
- `update` - Update a flag definition (default value, description, visibility)
- `rules` - Targeting rules: serve a different value to some users, or to a percentage of them
- `evaluate` (`eval`) - Show what a user would get, and why, in one environment
- `reshuffle` - Draw new rollout buckets for every rollout and split of this flag (never during an experiment)
- `set` - Set a flag value in one environment
- `clear` - Clear a flag override so the environment inherits the default
- `disable` - Kill switch: serve the default in this environment, ignoring any override
- `enable` - Re-enable a flag in this environment after a kill switch
- `restore` - Restore an archived flag
- `archive` - Archive a flag (callers fall back to their own default)
- `record-exposure` - Record which flags a running app actually evaluated
- `get-snapshot` - Every flag for the caller's environment, collapsed to what the evaluator needs
- `ofrep-server-bulk` - OFREP bulk evaluation for server-side OpenFeature SDKs
- `ofrep-server-single` - OFREP single-flag evaluation for server-side OpenFeature SDKs

### `flags list` (alias: `ls`)

List feature flags

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Show values for this environment | - | No |
| `--include-archived` | Include archived flags | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags get`

Show a feature flag and its per-environment values

**Usage:** `flags get <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `flags create`

Create a feature flag

**Usage:** `flags create <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-t, --type <type>` | Value type: bool, string, number, or json | - | Yes |
| `-d, --default <value>` | Default value, served when nothing more specific applies | - | Yes |
| `--description <text>` | What this flag controls | - | No |
| `--client-visible` | Allow this flag to be exposed to browsers (default: server-only) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags update`

Update a flag definition (default value, description, visibility)

**Usage:** `flags update <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-d, --default <value>` | New default value | - | No |
| `--description <text>` | New description | - | No |
| `--client-visible` | Expose this flag to browsers | - | No |
| `--no-client-visible` | Make this flag server-only | - | No |
| `--variants <json\|@file>` | Replace the variants, as a JSON array of {key, value, description?} (or @path to a file) | - | No |
| `--exposure-rate <percent>` | Share of visitors (0-100) whose browser records a flag_exposure event; 0 turns it off | - | No |
| `--json` | Output in JSON format | - | No |

### `flags rules`

Targeting rules: serve a different value to some users, or to a percentage of them

**Subcommands:**

- `list` (`ls`) - Show the ordered rules of a flag in one environment
- `set` - Replace the rules of a flag in one environment, from a JSON array (or @path to a file, or - for stdin)
- `clear` - Remove every rule of a flag in one environment

#### `flags rules list` (alias: `ls`)

Show the ordered rules of a flag in one environment

**Usage:** `flags rules list <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output the rules as JSON (the format `rules set` reads) | - | No |

#### `flags rules set`

Replace the rules of a flag in one environment, from a JSON array (or @path to a file, or - for stdin)

**Usage:** `flags rules set <key> <rules> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `flags rules clear`

Remove every rule of a flag in one environment

**Usage:** `flags rules clear <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags evaluate` (alias: `eval`)

Show what a user would get, and why, in one environment

**Usage:** `flags evaluate <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `-s, --subject <key>` | Bucketing subject, e.g. a user id | - | No |
| `-a, --attr <name=value>` | Targeting attribute (repeatable) | `` | No |
| `--json` | Output in JSON format | - | No |

### `flags reshuffle`

Draw new rollout buckets for every rollout and split of this flag (never during an experiment)

**Usage:** `flags reshuffle <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip the confirmation | - | No |

### `flags set`

Set a flag value in one environment

**Usage:** `flags set <key> <value> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output in JSON format | - | No |

### `flags clear`

Clear a flag override so the environment inherits the default

**Usage:** `flags clear <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags disable`

Kill switch: serve the default in this environment, ignoring any override

**Usage:** `flags disable <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags enable`

Re-enable a flag in this environment after a kill switch

**Usage:** `flags enable <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags restore`

Restore an archived flag

**Usage:** `flags restore <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `flags archive`

Archive a flag (callers fall back to their own default)

**Usage:** `flags archive <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `flags record-exposure`

Record which flags a running app actually evaluated

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--keys <value>` | Flag keys evaluated since the last report (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags get-snapshot`

Every flag for the caller's environment, collapsed to what the evaluator needs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--environment-id <n>` | Required only when the calling token is project-wide rather than scoped to a single environment | - | No |
| `--json` | Output in JSON format | - | No |

### `flags ofrep-server-bulk`

OFREP bulk evaluation for server-side OpenFeature SDKs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--context <json>` | Body field context (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags ofrep-server-single`

OFREP single-flag evaluation for server-side OpenFeature SDKs

**Usage:** `flags ofrep-server-single <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--context <json>` | Body field context (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
