<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `flags` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `flags` (alias: `flag`)

Manage feature flags (runtime config that changes without a redeploy)

**Subcommands:**

- `list` (`ls`) - List feature flags
- `get` - Show a feature flag and its per-environment values
- `create` - Create a feature flag
- `update` - Update a flag definition (default value, description, visibility)
- `set` - Set a flag value in one environment
- `clear` - Clear a flag override so the environment inherits the default
- `disable` - Kill switch: serve the default in this environment, ignoring any override
- `enable` - Re-enable a flag in this environment after a kill switch
- `restore` - Restore an archived flag
- `archive` - Archive a flag (callers fall back to their own default)

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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `flags create`

Create a feature flag

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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-d, --default <value>` | New default value | - | No |
| `--description <text>` | New description | - | No |
| `--client-visible` | Expose this flag to browsers | - | No |
| `--no-client-visible` | Make this flag server-only | - | No |
| `--json` | Output in JSON format | - | No |

### `flags set`

Set a flag value in one environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output in JSON format | - | No |

### `flags clear`

Clear a flag override so the environment inherits the default

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags disable`

Kill switch: serve the default in this environment, ignoring any override

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags enable`

Re-enable a flag in this environment after a kill switch

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags restore`

Restore an archived flag

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `flags archive`

Archive a flag (callers fall back to their own default)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
