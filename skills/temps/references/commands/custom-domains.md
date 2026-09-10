<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `custom-domains` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `custom-domains` (alias: `cdom`)

Manage project custom domains

**Subcommands:**

- `list` (`ls`) - List custom domains for a project
- `create` (`add`) - Create a custom domain for a project
- `show` - Show custom domain details
- `update` - Update a custom domain
- `remove` (`rm`) - Remove a custom domain
- `link-cert` - Link a custom domain to a certificate

### `custom-domains list` (alias: `ls`)

List custom domains for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `custom-domains create` (alias: `add`)

Create a custom domain for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | No |
| `--environment-id <id>` | Environment ID | `0` | No |
| `--branch <branch>` | Branch name | - | No |
| `--redirect-to <url>` | Redirect target URL | - | No |
| `--status-code <code>` | HTTP status code for redirects | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `custom-domains show`

Show custom domain details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `custom-domains update`

Update a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `-d, --domain <domain>` | New domain name | - | No |
| `--environment-id <id>` | New environment ID | - | No |
| `--branch <branch>` | New branch name | - | No |
| `--redirect-to <url>` | New redirect target URL | - | No |
| `--status-code <code>` | New HTTP status code for redirects | - | No |

### `custom-domains remove` (alias: `rm`)

Remove a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `custom-domains link-cert`

Link a custom domain to a certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `--certificate-id <id>` | Certificate ID | - | Yes |
