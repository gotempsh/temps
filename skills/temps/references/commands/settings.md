<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `settings` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`settings show`](#settings-show-alias-get)
- [`settings update`](#settings-update-alias-set)
- [`settings geo-status`](#settings-geo-status)
- [`settings set-external-url`](#settings-set-external-url)
- [`settings set-preview-domain`](#settings-set-preview-domain)
- [`settings fields`](#settings-fields)
- [`settings apply`](#settings-apply)
- [`settings admin-gate`](#settings-admin-gate)
- [`settings telemetry`](#settings-telemetry)
- [`settings ci-builder`](#settings-ci-builder)
- [`settings disk-status`](#settings-disk-status)
- [`settings public-ingress`](#settings-public-ingress)
- [`settings refresh-routes`](#settings-refresh-routes)
- [`settings weekly-digest`](#settings-weekly-digest)
- [`settings oidc`](#settings-oidc)

## `settings`

Manage platform settings

**Subcommands:**

- `show` (`get`) - Show current platform settings
- `update` (`set`) - Update platform settings
- `geo-status` - Show the freshness of the geolocation (GeoLite2) database
- `set-external-url` - Set the external URL for the platform
- `set-preview-domain` - Set the preview domain pattern
- `fields` - List every instance setting, or print the value at a field path (e.g. shield, otel_ingest.max_spans_per_second)
- `apply` - Change any instance setting by field path: field=value (JSON or text), field:=text, or a bare field with --value-stdin
- `admin-gate` - Show, or change with flags, which hosts and source IPs may reach the console and API listener
- `telemetry` - Show anonymous telemetry status, or turn it on/off with --enable/--disable
- `ci-builder` - Configure the external CI builder that runs builds off the control plane
- `disk-status` - Show the control plane's disk usage and alert threshold
- `public-ingress` - Show where third parties (webhooks, email tracking) call back into Temps, and whether it is reachable
- `refresh-routes` - Rebuild the proxy route table now instead of waiting for the next change
- `weekly-digest` - Generate and send the weekly digest now
- `oidc` - Oidc commands

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
| `--max-memory-limit-mb <mb>` | Ceiling on a project/environment memory limit override, in MB (0 = no ceiling) | - | No |
| `--max-concurrent-connections-ceiling <count>` | Ceiling on a project/environment concurrent-connection override (0 = no ceiling) | - | No |
| `--allow-unlimited-timeouts <enabled>` | Whether projects may set a timeout of 0, i.e. no timeout (true/false) | - | No |
| `--console-force-https <mode>` | Redirect the console host to HTTPS: auto (once a cert exists), always, or never | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `settings geo-status`

Show the freshness of the geolocation (GeoLite2) database

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

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

### `settings fields`

List every instance setting, or print the value at a field path (e.g. shield, otel_ingest.max_spans_per_second)

**Usage:** `settings fields [path] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings apply`

Change any instance setting by field path: field=value (JSON or text), field:=text, or a bare field with --value-stdin

**Usage:** `settings apply [assignments...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from-file <path>` | Merge a JSON object of settings from a file (- for stdin) | - | No |
| `--value-stdin` | Read the value of the one bare field from stdin (for secrets) | - | No |
| `-y, --yes` | Do not ask before changing a setting that can lock you out | - | No |
| `--json` | Print the saved settings response as JSON | - | No |

### `settings admin-gate`

Show, or change with flags, which hosts and source IPs may reach the console and API listener

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--allowed-host <host>` | Host header to allow (repeatable; replaces the list) | - | No |
| `--allowed-ip <ip-or-cidr>` | Source IP or CIDR to allow (repeatable; replaces the list) | - | No |
| `--clear-hosts` | Allow any Host header | - | No |
| `--clear-ips` | Allow any source address | - | No |
| `--trust-forwarded-for` | Trust X-Forwarded-For from loopback peers | - | No |
| `--no-trust-forwarded-for` | Ignore X-Forwarded-For | - | No |
| `-y, --yes` | Do not ask before saving a gate that may lock you out | - | No |
| `--json` | Output in JSON format | - | No |

### `settings telemetry`

Show anonymous telemetry status, or turn it on/off with --enable/--disable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Send anonymous telemetry | - | No |
| `--disable` | Stop sending anonymous telemetry | - | No |
| `--json` | Output in JSON format | - | No |

### `settings ci-builder`

Configure the external CI builder that runs builds off the control plane

**Subcommands:**

- `show` - Show whether the CI builder is configured, and its settings (credentials are never returned)
- `set` - Save CI builder settings from --from-file and/or field=value assignments (e.g. limits.concurrency=2)
- `provision` - Create the builder repository and its pinned runner shim at the configured provider

#### `settings ci-builder show`

Show whether the CI builder is configured, and its settings (credentials are never returned)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings ci-builder set`

Save CI builder settings from --from-file and/or field=value assignments (e.g. limits.concurrency=2)

**Usage:** `settings ci-builder set [assignments...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from-file <path>` | JSON object of CI builder settings (- for stdin) | - | No |
| `--dispatch-token-stdin` | Read the dispatch token from stdin | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings ci-builder provision`

Create the builder repository and its pinned runner shim at the configured provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |

### `settings disk-status`

Show the control plane's disk usage and alert threshold

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings public-ingress`

Show where third parties (webhooks, email tracking) call back into Temps, and whether it is reachable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings refresh-routes`

Rebuild the proxy route table now instead of waiting for the next change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings weekly-digest`

Generate and send the weekly digest now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings oidc`

Oidc commands

**Subcommands:**

- `list-providers` - List oidc providers
- `create-provider` - Create oidc provider
- `delete-provider` - Delete oidc provider
- `update-provider` - Update oidc provider
- `list-role-mappings` - List oidc role mappings
- `create-role-mapping` - Create oidc role mapping
- `test-provider` - Test oidc provider
- `list-provider-users` - List oidc provider users
- `delete-role-mapping` - Delete oidc role mapping

#### `settings oidc list-providers`

List oidc providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc create-provider`

Create oidc provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--client-id <value>` | Body field client_id [required] | - | No |
| `--client-secret-stdin` | A credential [required] (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential [required] (read from a file) | - | No |
| `--default-role <value>` | Body field default_role | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--group-claim <value>` | Body field group_claim | - | No |
| `--issuer-url <value>` | Body field issuer_url [required] | - | No |
| `--jit-provisioning` | Body field jit_provisioning | - | No |
| `--no-jit-provisioning` | Set jit_provisioning to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--role-claim <value>` | Body field role_claim | - | No |
| `--scopes <value>` | Body field scopes | - | No |
| `--template <value>` | Body field template | - | No |
| `--trust-idp-email` | Defaults false | - | No |
| `--no-trust-idp-email` | Set trust_idp_email to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc delete-provider`

Delete oidc provider

**Usage:** `settings oidc delete-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc update-provider`

Update oidc provider

**Usage:** `settings oidc update-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--client-id <value>` | Body field client_id | - | No |
| `--client-secret-stdin` | A credential (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential (read from a file) | - | No |
| `--default-role <value>` | Body field default_role | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--group-claim <value>` | Body field group_claim | - | No |
| `--issuer-url <value>` | Body field issuer_url | - | No |
| `--jit-provisioning` | Body field jit_provisioning | - | No |
| `--no-jit-provisioning` | Set jit_provisioning to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--role-claim <value>` | Body field role_claim | - | No |
| `--scopes <value>` | Body field scopes | - | No |
| `--template <value>` | Body field template | - | No |
| `--trust-idp-email` | Body field trust_idp_email | - | No |
| `--no-trust-idp-email` | Set trust_idp_email to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc list-role-mappings`

List oidc role mappings

**Usage:** `settings oidc list-role-mappings <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc create-role-mapping`

Create oidc role mapping

**Usage:** `settings oidc create-role-mapping <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--idp-group <value>` | Body field idp_group [required] | - | No |
| `--priority <n>` | Body field priority [required] | - | No |
| `--role <value>` | Body field role [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc test-provider`

Test oidc provider

**Usage:** `settings oidc test-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc list-provider-users`

List oidc provider users

**Usage:** `settings oidc list-provider-users <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc delete-role-mapping`

Delete oidc role mapping

**Usage:** `settings oidc delete-role-mapping <mapping-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |
