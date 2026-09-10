<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `otel-forward` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `otel-forward`

Manage OTel forwarding destinations that relay ingested traces, metrics, and logs to an external OTLP-compatible collector

**Subcommands:**

- `list` (`ls`) - List OTel forwarding destinations for a project
- `create` - Create a new OTel forwarding destination
- `show` - Show OTel forwarding destination details
- `update` - Update an OTel forwarding destination
- `remove` - Remove an OTel forwarding destination
- `test` - Send a test delivery to an OTel forwarding destination
- `instance-default` - Manage instance-wide default forwarding destinations — applied automatically to any project with zero enabled destinations of its own. As soon as a project has one of its own destinations, instance defaults stop applying to that project.

### `otel-forward list` (alias: `ls`)

List OTel forwarding destinations for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `otel-forward create`

Create a new OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--name <name>` | Destination name | - | Yes |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | Yes |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | Yes |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces (default: true) | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics (default: true) | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs (default: true) | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Create the destination enabled (default) | - | No |
| `--disabled` | Create the destination disabled | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--json` | Output in JSON format | - | No |

### `otel-forward show`

Show OTel forwarding destination details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel-forward update`

Update an OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Destination name | - | No |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | No |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | No |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Enable the destination | - | No |
| `--disabled` | Disable the destination | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--no-allow-private-network` | Disallow private/loopback/link-local endpoint URLs | - | No |
| `--json` | Output in JSON format | - | No |

### `otel-forward remove`

Remove an OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `otel-forward test`

Send a test delivery to an OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel-forward instance-default`

Manage instance-wide default forwarding destinations — applied automatically to any project with zero enabled destinations of its own. As soon as a project has one of its own destinations, instance defaults stop applying to that project.

**Subcommands:**

- `list` (`ls`) - List instance-wide default forwarding destinations
- `create` - Create a new instance-wide default forwarding destination
- `show` - Show instance default destination details
- `update` - Update an instance default forwarding destination
- `remove` - Remove an instance default forwarding destination
- `test` - Send a test delivery to an instance default forwarding destination

#### `otel-forward instance-default list` (alias: `ls`)

List instance-wide default forwarding destinations

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default create`

Create a new instance-wide default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Destination name | - | Yes |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | Yes |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | Yes |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces (default: true) | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics (default: true) | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs (default: true) | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Create the destination enabled (default) | - | No |
| `--disabled` | Create the destination disabled | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default show`

Show instance default destination details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default update`

Update an instance default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Destination name | - | No |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | No |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | No |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Enable the destination | - | No |
| `--disabled` | Disable the destination | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--no-allow-private-network` | Disallow private/loopback/link-local endpoint URLs | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default remove`

Remove an instance default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `otel-forward instance-default test`

Send a test delivery to an instance default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
