<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `providers` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`providers list`](#providers-list-alias-ls)
- [`providers add`](#providers-add)
- [`providers remove`](#providers-remove-alias-rm)
- [`providers show`](#providers-show)
- [`providers activate`](#providers-activate)
- [`providers deactivate`](#providers-deactivate)
- [`providers safe-delete`](#providers-safe-delete)
- [`providers deletion-check`](#providers-deletion-check)
- [`providers git`](#providers-git)
- [`providers connections`](#providers-connections-alias-conn)
- [`providers public-repos`](#providers-public-repos)
- [`providers repos`](#providers-repos)

## `providers` (alias: `provider`)

Manage Git providers

**Subcommands:**

- `list` (`ls`) - List configured Git providers
- `add` - Add a new Git provider
- `remove` (`rm`) - Remove a Git provider
- `show` - Show Git provider details
- `activate` - Activate a Git provider
- `deactivate` - Deactivate a Git provider
- `safe-delete` - Safely delete a Git provider (checks dependencies first)
- `deletion-check` - Check if a Git provider can be safely deleted
- `git` - Manage Git providers
- `connections` (`conn`) - Manage Git provider connections
- `public-repos` - Public repos commands
- `repos` - Repos commands

### `providers list` (alias: `ls`)

List configured Git providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers add`

Add a new Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --provider <provider>` | Provider type (github, gitlab, bitbucket, gitea, generic) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-t, --token <token>` | Personal access token (or Bitbucket access token / app password) | - | No |
| `--base-url <url>` | Instance base URL (GitLab/Gitea self-hosted; required for gitea) | - | No |
| `--username <username>` | Bitbucket username (selects app-password auth) | - | No |
| `--password <password>` | Bitbucket app password (used with --username) | - | No |
| `--clone-url <url>` | HTTPS clone URL (generic provider) | - | No |
| `--token-username <username>` | HTTP Basic username for the token (generic; default x-access-token) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--token-file <path>` | Read the access token from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the access token from stdin instead of --token | - | No |
| `--password-file <path>` | Read the Bitbucket app password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the Bitbucket app password from stdin instead of --password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `providers remove` (alias: `rm`)

Remove a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `providers show`

Show Git provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `providers activate`

Activate a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `providers deactivate`

Deactivate a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `providers safe-delete`

Safely delete a Git provider (checks dependencies first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `providers deletion-check`

Check if a Git provider can be safely deleted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `providers git`

Manage Git providers

**Subcommands:**

- `connect` - Connect a Git provider (github, gitlab, bitbucket, gitea, generic)
- `repos` - List available repositories
- `complete-connection-reauthorization` - Complete connection reauthorization
- `reauthorize-connection` - Reauthorize connection
- `create` - Create a new git provider configuration
- `start-github-manifest-flow` - Start creating a GitHub App from a manifest
- `create-gitlab-oauth` - Create a GitLab OAuth provider
- `update-credentials` - Partially update credentials for an existing git provider
- `list-repositories-by` - List all repositories for a specific provider
- `get-all-repositories-by-name` - Get all repositories with same owner/name from all git providers
- `get-repository-preset-by-name` - Get repository preset by owner and name
- `get-repository-compose-services-live` - Parse a compose file's services for a connected repository, live
- `get-repository-compose-preview` - Render a redacted effective Compose preview for a connected repository
- `get-repository-env-example-live` - Detect and parse a '.env.example'-style file for a connected repository
- `get-repository-by-id` - Get repository by ID

#### `providers git connect`

Connect a Git provider (github, gitlab, bitbucket, gitea, generic)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --provider <provider>` | Provider type (github, gitlab, bitbucket, gitea, generic) | - | Yes |
| `-n, --name <name>` | Provider name | - | No |
| `-t, --token <token>` | Personal access token (or Bitbucket access token / app password) | - | No |
| `--base-url <url>` | Instance base URL (GitLab/Gitea self-hosted; required for gitea) | - | No |
| `--username <username>` | Bitbucket username (selects app-password auth) | - | No |
| `--password <password>` | Bitbucket app password (used with --username) | - | No |
| `--clone-url <url>` | HTTPS clone URL (generic provider) | - | No |
| `--token-username <username>` | HTTP Basic username for the token (generic; default x-access-token) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--token-file <path>` | Read the access token from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the access token from stdin instead of --token | - | No |
| `--password-file <path>` | Read the Bitbucket app password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the Bitbucket app password from stdin instead of --password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `providers git repos`

List available repositories

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID (lists every synced repository if omitted) | - | No |
| `--json` | Output in JSON format | - | No |
| `--search <term>` | Search repositories by name | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (max: 100) | - | No |
| `--sort <field>` | Sort by field (name, created_at, updated_at, stars) | - | No |
| `--direction <dir>` | Sort direction: asc or desc | - | No |
| `--language <lang>` | Filter by programming language | - | No |
| `--owner <owner>` | Filter by repository owner | - | No |
| `--updated-after <value>` | Only repositories updated on or after this time (RFC 3339) | - | No |
| `--preset <value>` | Only repositories detected as this preset (e.g. nextjs); not with --id | - | No |
| `--git-provider-connection-id <n>` | Only repositories of this connection; not with --id | - | No |

#### `providers git complete-connection-reauthorization`

Complete connection reauthorization

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <value>` | Empty when the user cancelled at the provider; the state is still consumed [required] | - | No |
| `--state <value>` | Body field state [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git reauthorize-connection`

Reauthorize connection

**Usage:** `providers git reauthorize-connection <connection-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers git create`

Create a new git provider configuration

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--provider-type <value>` | Body field provider_type [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git start-github-manifest-flow`

Start creating a GitHub App from a manifest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers git create-gitlab-oauth`

Create a GitLab OAuth provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | Body field base_url | - | No |
| `--client-id <value>` | Body field client_id [required] | - | No |
| `--client-secret-stdin` | A credential [required] (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential [required] (read from a file) | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--redirect-uri <value>` | Body field redirect_uri [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git update-credentials`

Partially update credentials for an existing git provider

**Usage:** `providers git update-credentials <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--app-id <value>` | Application ID (GitHub App integer as string; GitLab App string) | - | No |
| `--app-secret-stdin` | A credential (read from stdin) | - | No |
| `--app-secret-file <path>` | A credential (read from a file) | - | No |
| `--client-id <value>` | OAuth client ID (GitLab OAuth, GitHub App) | - | No |
| `--client-secret-stdin` | A credential (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential (read from a file) | - | No |
| `--password-stdin` | A credential (read from stdin) | - | No |
| `--password-file <path>` | A credential (read from a file) | - | No |
| `--private-key-stdin` | A credential (read from stdin) | - | No |
| `--private-key-file <path>` | A credential (read from a file) | - | No |
| `--public-key <value>` | SSH public key (one OpenSSH line) of an SSH-key provider | - | No |
| `--redirect-uri <value>` | OAuth redirect URI (GitLab OAuth / GitLab App) | - | No |
| `--token-stdin` | A credential (read from stdin) | - | No |
| `--token-file <path>` | A credential (read from a file) | - | No |
| `--username <value>` | Username of a basic-auth provider (generic Git server, Bitbucket app password) | - | No |
| `--webhook-secret-stdin` | A credential (read from stdin) | - | No |
| `--webhook-secret-file <path>` | A credential (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git list-repositories-by`

List all repositories for a specific provider

**Usage:** `providers git list-repositories-by <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number for pagination | - | No |
| `--per-page <n>` | Number of items per page (max 100) | - | No |
| `--sort <value>` | Sort field (name, created_at, updated_at, stars, watchers, size, issues) | - | No |
| `--direction <value>` | Sort direction (asc, desc) | - | No |
| `--search <value>` | Search term to filter repositories | - | No |
| `--owner <value>` | Filter by repository owner | - | No |
| `--language <value>` | Filter by programming language | - | No |
| `--private` | Filter by private status (true/false) | - | No |
| `--no-private` | Set private to false | - | No |
| `--preset <value>` | Cached default-branch preset slug; __undetected__ means not inspected | - | No |
| `--updated-after <value>` | Updated on or after this RFC3339 timestamp | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-all-repositories-by-name`

Get all repositories with same owner/name from all git providers

**Usage:** `providers git get-all-repositories-by-name <owner> <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-preset-by-name`

Get repository preset by owner and name

**Usage:** `providers git get-repository-preset-by-name <owner> <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Git branch to check (defaults to repository's default branch) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-compose-services-live`

Parse a compose file's services for a connected repository, live

**Usage:** `providers git get-repository-compose-services-live <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Git branch to check (defaults to repository's default branch) | - | No |
| `--path <value>` | Compose file path to fetch and parse [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-compose-preview`

Render a redacted effective Compose preview for a connected repository

**Usage:** `providers git get-repository-compose-preview <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Body field branch | - | No |
| `--compose-override <value>` | Body field composeOverride | - | No |
| `--environment-id <n>` | Environment whose variables resolve '${VAR}'; defaults to the project's production environment | - | No |
| `--excluded-services <value>` | Body field excludedServices (repeatable) | - | No |
| `--path <value>` | Body field path [required] | - | No |
| `--preview-policy <json>` | Advisory preview only (JSON) | - | No |
| `--project-id <n>` | The project this Compose file deploys | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-env-example-live`

Detect and parse a '.env.example'-style file for a connected repository

**Usage:** `providers git get-repository-env-example-live <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Git branch to check (defaults to repository's default branch) | - | No |
| `--root-directory <value>` | Project root directory to search (defaults to repository root) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-by-id`

Get repository by ID

**Usage:** `providers git get-repository-by-id <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers connections` (alias: `conn`)

Manage Git provider connections

**Subcommands:**

- `list` (`ls`) - List all Git connections
- `get` - Show one Git connection: account, health and sync state
- `show` - Show connection details for a provider
- `delete` (`rm`) - Delete a Git connection
- `activate` - Activate a Git connection
- `deactivate` - Deactivate a Git connection
- `sync` - Sync repositories for a Git connection
- `update-token` - Update access token for a Git connection
- `validate` - Validate a Git connection
- `run-health-check` - Run an on-demand health check for a git connection
- `create-repository` - Create an empty repository through one of your git connections
- `commit-repository-files` - Commit files to a repository through one of your git connections
- `open-repository-pull-request` - Open a pull request through one of your git connections

#### `providers connections list` (alias: `ls`)

List all Git connections

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (default: 30, max: 100) | - | No |
| `--sort <field>` | Sort by field (created_at, updated_at, account_name) | - | No |
| `--direction <dir>` | Sort direction: asc or desc (default: desc) | - | No |

#### `providers connections get`

Show one Git connection: account, health and sync state

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `providers connections show`

Show connection details for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `providers connections delete` (alias: `rm`)

Delete a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `providers connections activate`

Activate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections deactivate`

Deactivate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections sync`

Sync repositories for a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections update-token`

Update access token for a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `-t, --token <token>` | New access token (prefer --token-file or --token-stdin, which keep it out of shell history) | - | No |
| `--refresh-token <token>` | New refresh token, for providers whose access tokens expire (prefer --refresh-token-file or --refresh-token-stdin) | - | No |
| `--token-file <path>` | Read the new access token from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the new access token from stdin instead of --token | - | No |
| `--refresh-token-file <path>` | Read the new refresh token from a file (- reads stdin) instead of --refresh-token | - | No |
| `--refresh-token-stdin` | Read the new refresh token from stdin instead of --refresh-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `providers connections validate`

Validate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `providers connections run-health-check`

Run an on-demand health check for a git connection

**Usage:** `providers connections run-health-check <connection-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers connections create-repository`

Create an empty repository through one of your git connections

**Usage:** `providers connections create-repository <connection-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--description <value>` | Short description, up to 350 characters | - | No |
| `--name <value>` | Repository name: letters, digits, '-', '_' and '.', up to 100 characters [required] | - | No |
| `--owner <value>` | Account or organization to create it under | - | No |
| `--private` | Whether the repository is private | - | No |
| `--no-private` | Set private to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers connections commit-repository-files`

Commit files to a repository through one of your git connections

**Usage:** `providers connections commit-repository-files <connection-id> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Branch to commit to | - | No |
| `--files <json>` | Files to add or overwrite (1-100, at most 5 MiB in total) (JSON) [required] | - | No |
| `--message <value>` | Commit message (at most 500 bytes) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers connections open-repository-pull-request`

Open a pull request through one of your git connections

**Usage:** `providers connections open-repository-pull-request <connection-id> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-branch <value>` | Branch to merge into | - | No |
| `--body-body <value>` | Body field body | - | No |
| `--head-branch <value>` | Branch holding the changes [required] | - | No |
| `--title <value>` | Body field title [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `providers public-repos`

Public repos commands

**Subcommands:**

- `get-repository` - Get information about a public repository (supports GitHub and GitLab)
- `get-branches` - Get branches for a public repository (supports GitHub and GitLab)
- `get-branch` - Look up one branch of a public repository exactly, even past the branch listing's cap (404 if it does not exist)
- `get-compose-services` - Parse a compose file's services for a public repository (supports GitHub and GitLab)
- `get-compose-preview` - Render a redacted effective Compose preview for a public repository
- `detect-env-example` - Detect and parse a '.env.example'-style file for a public repository (supports GitHub and GitLab)

#### `providers public-repos get-repository`

Get information about a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos get-repository <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-branches`

Get branches for a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos get-branches <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-branch`

Look up one branch of a public repository exactly, even past the branch listing's cap (404 if it does not exist)

**Usage:** `providers public-repos get-branch <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--name <value>` | Branch name to look up [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-compose-services`

Parse a compose file's services for a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos get-compose-services <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--branch <value>` | Branch to read the compose file from (default: repository's default branch) | - | No |
| `--path <value>` | Compose file path to fetch and parse (from the 'compose_files' list '/preset' already returned, or a... [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-compose-preview`

Render a redacted effective Compose preview for a public repository

**Usage:** `providers public-repos get-compose-preview <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--branch <value>` | Body field branch | - | No |
| `--compose-override <value>` | Body field composeOverride | - | No |
| `--excluded-services <value>` | Body field excludedServices (repeatable) | - | No |
| `--path <value>` | Body field path [required] | - | No |
| `--preview-policy <json>` | Advisory preview only (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos detect-env-example`

Detect and parse a '.env.example'-style file for a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos detect-env-example <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--branch <value>` | Branch name to inspect (default: repository's default branch) | - | No |
| `--root-directory <value>` | Project root directory to search (default: repository root) | - | No |
| `--json` | Output in JSON format | - | No |

### `providers repos`

Repos commands

**Subcommands:**

- `get-repository-branches` - Get repository branches
- `get-repository-branch` - Look up one branch of a repository exactly, even past the branch listing's cap (404 if it does not exist)
- `get-repository-tags` - Get repository tags
- `get-branches-by-repository-id` - Get repository branches by repository ID
- `check-commit-exists` - Check if a commit exists in a repository
- `get-tags-by-repository-id` - Get repository tags by repository ID

#### `providers repos get-repository-branches`

Get repository branches

**Usage:** `providers repos get-repository-branches <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Git provider connection ID (required when multiple connections have the same repo) [required] | - | No |
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos get-repository-branch`

Look up one branch of a repository exactly, even past the branch listing's cap (404 if it does not exist)

**Usage:** `providers repos get-repository-branch <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Git provider connection ID [required] | - | No |
| `--name <value>` | Branch name to look up [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos get-repository-tags`

Get repository tags

**Usage:** `providers repos get-repository-tags <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Git provider connection ID (required when multiple connections have the same repo) [required] | - | No |
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos get-branches-by-repository-id`

Get repository branches by repository ID

**Usage:** `providers repos get-branches-by-repository-id <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos check-commit-exists`

Check if a commit exists in a repository

**Usage:** `providers repos check-commit-exists <repository-id> <commit-sha> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers repos get-tags-by-repository-id`

Get repository tags by repository ID

**Usage:** `providers repos get-tags-by-repository-id <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |
