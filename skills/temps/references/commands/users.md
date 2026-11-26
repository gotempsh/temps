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
- `reset-password` - Reset another user's password to a generated temporary one. The user is signed out of every browser session and must choose a new password at next sign-in
- `reset-mfa` - Remove another user's MFA (authenticator and recovery codes) after they lost both. The user is signed out of every session. Password sign-in is blocked when require_mfa_for_admins applies; arrange SSO access or a temporary policy change before resetting MFA. Otherwise, sign in with a password to enroll again
- `role` - Manage user roles
- `update-self` - Update current user's information
- `disable-mfa` - Disable mfa
- `setup-mfa` - Setup mfa
- `verify-and-enable-mfa` - Verify and enable mfa
- `change-password-self` - Change password self
- `update` - Update user information (admin only)

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
| `--must-change-password` | Make the user choose a new password at first sign-in | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--password-file <path>` | Read the password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the password from stdin instead of --password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

### `users reset-password`

Reset another user's password to a generated temporary one. The user is signed out of every browser session and must choose a new password at next sign-in

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompt (for automation) | - | No |

### `users reset-mfa`

Remove another user's MFA (authenticator and recovery codes) after they lost both. The user is signed out of every session. Password sign-in is blocked when require_mfa_for_admins applies; arrange SSO access or a temporary policy change before resetting MFA. Otherwise, sign in with a password to enroll again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompt (for automation) | - | No |

### `users role`

Manage user roles

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--add <role>` | Add a role to user | - | No |
| `--remove <role>` | Remove a role from user | - | No |

### `users update-self`

Update current user's information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--email <value>` | Body field email | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users disable-mfa`

Disable mfa

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <value>` | Body field code [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `users setup-mfa`

Setup mfa

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--current-password-stdin` | Required to enroll MFA on an account that has a password set (read from stdin) | - | No |
| `--current-password-file <path>` | Required to enroll MFA on an account that has a password set (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users verify-and-enable-mfa`

Verify and enable mfa

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <value>` | Body field code [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users change-password-self`

Change password self

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--current-password-stdin` | Body field current_password [required] (read from stdin) | - | No |
| `--current-password-file <path>` | Body field current_password [required] (read from a file) | - | No |
| `--mfa-code <value>` | TOTP code (or recovery code) | - | No |
| `--new-password-stdin` | Body field new_password [required] (read from stdin) | - | No |
| `--new-password-file <path>` | Body field new_password [required] (read from a file) | - | No |
| `--revoke-other-sessions` | When true, every session OTHER than the one making this request is revoked | - | No |
| `--no-revoke-other-sessions` | Set revoke_other_sessions to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users update`

Update user information (admin only)

**Usage:** `users update <user-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--email <value>` | Body field email | - | No |
| `--name <value>` | Deprecated: the old name of 'username', still accepted | - | No |
| `--username <value>` | The new username, as in 'CreateUserRequest' | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
