<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `users` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `users`

Manage platform users

**Subcommands:**

- `list` (`ls`) - List all users
- `create` (`add`) - Create a new user
- `me` - Show current user info
- `remove` (`rm`) - Remove a user
- `restore` - Restore a deleted user
- `role` - Manage user roles

### `users list` (alias: `ls`)

List all users

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `users create` (alias: `add`)

Create a new user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --username <username>` | Username | - | No |
| `-e, --email <email>` | Email address | - | No |
| `-p, --password <password>` | Password (if not provided, invite email will be sent) | - | No |
| `-r, --roles <roles>` | Comma-separated roles (admin, user) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `users me`

Show current user info

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `users remove` (alias: `rm`)

Remove a user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `users restore`

Restore a deleted user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |

### `users role`

Manage user roles

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--add <role>` | Add a role to user | - | No |
| `--remove <role>` | Remove a role from user | - | No |
