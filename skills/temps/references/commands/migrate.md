<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `migrate` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `migrate` (alias: `imports`, `import`)

Migrate a project from another platform (Vercel, Coolify, Dokploy, CapRover, Portainer, Kubernetes, Docker) into temps

**Subcommands:**

- `sources` (`ls`) - List available import sources
- `discover` - Discover workloads from a source
- `plan` - Discover a source, pick a workload, and show the import plan
- `run` - Guided end-to-end migration: discover, plan, review, and execute
- `execute` - Execute a previously created import plan by session ID
- `status` - Show a stored import session (the plan it was created with)

### `migrate sources` (alias: `ls`)

List available import sources

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `migrate discover`

Discover workloads from a source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --source <source>` | Import source (coolify, dokploy, caprover, portainer, kubernetes, kamal, docker) | - | No |
| `--token <token>` | API token / admin password for the source instance | - | No |
| `--base-url <url>` | Base URL of the source instance | - | No |
| `--username <name>` | Admin username (portainer source, defaults to "admin") | - | No |
| `--kubeconfig <path>` | Path to a kubeconfig file (kubernetes source) | - | No |
| `--deploy-yml <path>` | Path to config/deploy.yml (kamal source) | - | No |
| `--json` | Output in JSON format | - | No |
| `--name-pattern <value>` | Only workloads whose name matches this glob or pattern | - | No |
| `--status-filter <value>` | Only workloads in this state, e.g. running (repeatable) (repeatable) | - | No |
| `--workload-type-filter <value>` | Only workloads of this type, e.g. container or static-site (repeatable) (repeatable) | - | No |
| `--label-filter <json>` | Only workloads with these labels, as a JSON object, e.g. {"env":"prod"} (JSON) | - | No |
| `--limit <n>` | Return at most this many workloads | - | No |
| `--token-file <path>` | Read the source API token or admin password from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the source API token or admin password from stdin instead of --token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `migrate plan`

Discover a source, pick a workload, and show the import plan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --source <source>` | Import source | - | No |
| `-w, --workload <workload>` | Workload ID to import (skips the picker) | - | No |
| `--token <token>` | API token / admin password for the source instance | - | No |
| `--base-url <url>` | Base URL of the source instance | - | No |
| `--username <name>` | Admin username (portainer source, defaults to "admin") | - | No |
| `--kubeconfig <path>` | Path to a kubeconfig file (kubernetes source) | - | No |
| `--deploy-yml <path>` | Path to config/deploy.yml (kamal source) | - | No |
| `--name-pattern <value>` | Only workloads whose name matches this glob or pattern | - | No |
| `--status-filter <value>` | Only workloads in this state, e.g. running (repeatable) (repeatable) | - | No |
| `--workload-type-filter <value>` | Only workloads of this type, e.g. container or static-site (repeatable) (repeatable) | - | No |
| `--label-filter <json>` | Only workloads with these labels, as a JSON object, e.g. {"env":"prod"} (JSON) | - | No |
| `--limit <n>` | Return at most this many workloads | - | No |
| `--repository-id <n>` | Connected repository to detect the build preset from | - | No |
| `--token-file <path>` | Read the source API token or admin password from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the source API token or admin password from stdin instead of --token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `migrate run`

Guided end-to-end migration: discover, plan, review, and execute

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --source <source>` | Import source (vercel, coolify, dokploy, caprover, portainer, kubernetes, kamal, docker) | - | No |
| `-w, --workload <workload>` | Workload ID to import (skips the picker) | - | No |
| `--token <token>` | API token / admin password for the source instance | - | No |
| `--base-url <url>` | Base URL of the source instance | - | No |
| `--username <name>` | Admin username (portainer source, defaults to "admin") | - | No |
| `--kubeconfig <path>` | Path to a kubeconfig file (kubernetes source) | - | No |
| `--deploy-yml <path>` | Path to config/deploy.yml (kamal source) | - | No |
| `--project-name <name>` | Name for the new temps project (defaults to the source project name) | - | No |
| `--preset <preset>` | Build preset (defaults to "nixpacks" for git sources, "dockerfile" otherwise) | - | No |
| `--directory <dir>` | Project subdirectory | `.` | No |
| `--branch <branch>` | Branch to deploy | `main` | No |
| `--dry-run` | Plan only — do not create or deploy anything | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--name-pattern <value>` | Only workloads whose name matches this glob or pattern | - | No |
| `--status-filter <value>` | Only workloads in this state, e.g. running (repeatable) (repeatable) | - | No |
| `--workload-type-filter <value>` | Only workloads of this type, e.g. container or static-site (repeatable) (repeatable) | - | No |
| `--label-filter <json>` | Only workloads with these labels, as a JSON object, e.g. {"env":"prod"} (JSON) | - | No |
| `--limit <n>` | Return at most this many workloads | - | No |
| `--repository-id <n>` | Connected repository to detect the build preset from | - | No |
| `--token-file <path>` | Read the source API token or admin password from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the source API token or admin password from stdin instead of --token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `migrate execute`

Execute a previously created import plan by session ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session-id <id>` | Import session ID (from `imports plan`) | - | Yes |
| `--project-name <name>` | Name for the new temps project | - | No |
| `--preset <preset>` | Build preset | `nixpacks` | No |
| `--directory <dir>` | Project subdirectory | `.` | No |
| `--branch <branch>` | Branch to deploy | `main` | No |
| `--dry-run` | Plan only — do not create or deploy anything | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |

### `migrate status`

Show a stored import session (the plan it was created with)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session-id <id>` | Import session ID | - | Yes |
| `--json` | Output in JSON format | - | No |
