<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `vault` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`vault list-services`](#vault-list-services)
- [`vault create-service`](#vault-create-service)
- [`vault delete-service`](#vault-delete-service)
- [`vault get-service`](#vault-get-service)
- [`vault update-service`](#vault-update-service)
- [`vault sessions`](#vault-sessions)
- [`vault list`](#vault-list)
- [`vault create`](#vault-create)
- [`vault delete`](#vault-delete)
- [`vault get`](#vault-get)
- [`vault update`](#vault-update)
- [`vault list-bundles`](#vault-list-bundles)
- [`vault create-bundle`](#vault-create-bundle)
- [`vault delete-bundle`](#vault-delete-bundle)
- [`vault update-bundle`](#vault-update-bundle)
- [`vault get-eligibility`](#vault-get-eligibility)
- [`vault list-links`](#vault-list-links)
- [`vault create-link`](#vault-create-link)
- [`vault delete-link`](#vault-delete-link)
- [`vault project`](#vault-project)
- [`vault sandbox`](#vault-sandbox)
- [`vault capability`](#vault-capability)
- [`vault service-catalog`](#vault-service-catalog)

## `vault`

Access vaults, the services they hold and their sessions

**Subcommands:**

- `list-services` - List access services general
- `create-service` - Create access service general
- `delete-service` - Delete access service general
- `get-service` - Get access service general
- `update-service` - Update access service general
- `sessions` - Sessions commands
- `list` - List access vaults general
- `create` - Create access vault general
- `delete` - Delete access vault general
- `get` - Get access vault general
- `update` - Update access vault general
- `list-bundles` - List access bundles general
- `create-bundle` - Create access bundle general
- `delete-bundle` - Delete access bundle general
- `update-bundle` - Update access bundle general
- `get-eligibility` - Get vault eligibility
- `list-links` - List vault links
- `create-link` - Create vault link
- `delete-link` - Delete vault link
- `project` - Project commands
- `sandbox` - Sandbox access vaults
- `capability` - General access capability
- `service-catalog` - Service catalog

### `vault list-services`

List access services general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create-service`

Create access service general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--source-project-id <n>` | Query parameter source_project_id | - | No |
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete-service`

Delete access service general

**Usage:** `vault delete-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault get-service`

Get access service general

**Usage:** `vault get-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `vault update-service`

Update access service general

**Usage:** `vault update-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault sessions`

Sessions commands

**Subcommands:**

- `list` - List access sessions general
- `get` - Get access session general
- `revoke` - Revoke access session general
- `create` - Create access session general

#### `vault sessions list`

List access sessions general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--vault-id <value>` | Query parameter vault_id | - | No |
| `--sandbox-id <value>` | Filter by the sandbox's public ID; ownership still applies | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault sessions get`

Get access session general

**Usage:** `vault sessions get <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `vault sessions revoke`

Revoke access session general

**Usage:** `vault sessions revoke <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault sessions create`

Create access session general

**Usage:** `vault sessions create <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--bundle-id <value>` | Body field bundle_id [required] | - | No |
| `--bundle-version <n>` | Body field bundle_version [required] | - | No |
| `--consumer <json>` | Body field consumer (JSON) [required] | - | No |
| `--request-id <value>` | Stable for retries of this approval [required] | - | No |
| `--services <json>` | Exact policies the operator reviewed, one per bundle service (maximum 16) (JSON) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault list`

List access vaults general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create`

Create access vault general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete`

Delete access vault general

**Usage:** `vault delete <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault get`

Get access vault general

**Usage:** `vault get <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--sandbox-id <value>` | Query parameter sandbox_id | - | No |
| `--json` | Output in JSON format | - | No |

### `vault update`

Update access vault general

**Usage:** `vault update <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault list-bundles`

List access bundles general

**Usage:** `vault list-bundles <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--sandbox-id <value>` | Query parameter sandbox_id | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create-bundle`

Create access bundle general

**Usage:** `vault create-bundle <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete-bundle`

Delete access bundle general

**Usage:** `vault delete-bundle <vault-id> <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault update-bundle`

Update access bundle general

**Usage:** `vault update-bundle <vault-id> <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault get-eligibility`

Get vault eligibility

**Usage:** `vault get-eligibility <vault-id> <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `vault list-links`

List vault links

**Usage:** `vault list-links <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--project-ids <value>` | Query parameter project_ids | - | No |
| `--sandbox-ids <value>` | Query parameter sandbox_ids | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create-link`

Create vault link

**Usage:** `vault create-link <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Body field project_id | - | No |
| `--sandbox-id <value>` | Body field sandbox_id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete-link`

Delete vault link

**Usage:** `vault delete-link <vault-id> <link-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault project`

Project commands

**Subcommands:**

- `sessions` - Sessions commands
- `list` - List access vaults
- `create` - Create access vault
- `get` - Get access vault
- `update` - Update access vault
- `list-bundles` - List access bundles
- `create-bundle` - Create access bundle
- `update-bundle` - Update access bundle
- `create-session` - Create access session
- `project` - Project access vaults

#### `vault project sessions`

Sessions commands

**Subcommands:**

- `list` - List access sessions
- `get` - Get access session
- `revoke` - Revoke access session

##### `vault project sessions list`

List access sessions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--vault-id <value>` | Query parameter vault_id | - | No |
| `--sandbox-id <value>` | Filter by the sandbox's public ID; ownership still applies | - | No |
| `--json` | Output in JSON format | - | No |

##### `vault project sessions get`

Get access session

**Usage:** `vault project sessions get <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

##### `vault project sessions revoke`

Revoke access session

**Usage:** `vault project sessions revoke <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project list`

List access vaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project create`

Create access vault

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project get`

Get access vault

**Usage:** `vault project get <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project update`

Update access vault

**Usage:** `vault project update <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project list-bundles`

List access bundles

**Usage:** `vault project list-bundles <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project create-bundle`

Create access bundle

**Usage:** `vault project create-bundle <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project update-bundle`

Update access bundle

**Usage:** `vault project update-bundle <vault-id> <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project create-session`

Create access session

**Usage:** `vault project create-session <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--bundle-id <value>` | Body field bundle_id [required] | - | No |
| `--bundle-version <n>` | Body field bundle_version [required] | - | No |
| `--consumer <json>` | Body field consumer (JSON) [required] | - | No |
| `--request-id <value>` | Stable for retries of this approval [required] | - | No |
| `--services <json>` | Exact policies the operator reviewed, one per bundle service (maximum 16) (JSON) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project project`

Project access vaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault sandbox`

Sandbox access vaults

**Usage:** `vault sandbox <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault capability`

General access capability

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `vault service-catalog`

Service catalog

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--search <value>` | Query parameter search | - | No |
| `--id <value>` | Query parameter id | - | No |
| `--json` | Output in JSON format | - | No |
