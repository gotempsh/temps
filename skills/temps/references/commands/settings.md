<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `settings` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `settings`

Manage platform settings

**Subcommands:**

- `show` (`get`) - Show current platform settings
- `update` (`set`) - Update platform settings
- `set-external-url` - Set the external URL for the platform
- `set-preview-domain` - Set the preview domain pattern

### `settings show` (alias: `get`)

Show current platform settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings update` (alias: `set`)

Update platform settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --setting <setting>` | Setting to update (external_url, preview_domain, letsencrypt, rate_limiting, security_headers, screenshots) | - | No |
| `-v, --value <value>` | Value for the setting | - | No |
| `--external-url <url>` | External URL for the platform | - | No |
| `--preview-domain <domain>` | Preview domain pattern | - | No |
| `--letsencrypt-email <email>` | Let's Encrypt email | - | No |
| `--letsencrypt-mode <mode>` | Let's Encrypt mode (staging, production) | - | No |
| `--rate-limiting-enabled <enabled>` | Enable rate limiting (true/false) | - | No |
| `--rate-limiting-rpm <rpm>` | Requests per minute | - | No |
| `--screenshots-enabled <enabled>` | Enable screenshots (true/false) | - | No |
| `--max-request-timeout <seconds>` | Hard ceiling for all upstream request/idle timeouts, in seconds | - | No |
| `--default-http-timeout <seconds>` | Default timeout for regular HTTP requests, in seconds | - | No |
| `--default-sse-idle-timeout <seconds>` | Default idle timeout for SSE streams, in seconds | - | No |
| `--default-websocket-idle-timeout <seconds>` | Default idle timeout for WebSocket connections, in seconds | - | No |
| `--console-force-https <mode>` | Redirect the console host to HTTPS: auto (once a cert exists), always, or never | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `settings set-external-url`

Set the external URL for the platform

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--url <url>` | External URL | - | Yes |

### `settings set-preview-domain`

Set the preview domain pattern

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain <domain>` | Preview domain pattern | - | Yes |
