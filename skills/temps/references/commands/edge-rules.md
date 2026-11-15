<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `edge-rules` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `edge-rules` (alias: `edge-rule`)

Block, allow, challenge, redirect or log requests before they reach the app. Rules run top to bottom; the first match decides

**Subcommands:**

- `list` (`ls`) - List the rules in evaluation order
- `get` - Show one rule
- `create` - Add a rule. New rules start in log-only mode unless --mode enforce is given
- `update` - Change a rule; only the given parts change
- `delete` (`rm`) - Delete a rule
- `move` - Move a rule to another position
- `enable` - Enable a rule
- `disable` - Disable a rule (kept, never evaluated)
- `enforce` - Act on matches
- `log-only` - Only record matches, never act on them
- `test` - Dry-run a request against the saved rules (or a rules file) and show the trace
- `export` - Print the rules as a file apply accepts
- `apply` - Make the project rules match a file: rules are matched by name, changed ones updated, missing ones created, others deleted, then ordered as in the file

### `edge-rules list` (alias: `ls`)

List the rules in evaluation order

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules get`

Show one rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules create`

Add a rule. New rules start in log-only mode unless --mode enforce is given

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | JSON file with the rule: {name?, expression, action, mode?, enabled?, environment_id?} | - | No |
| `--name <name>` | Rule name (generated from the rule when omitted) | - | No |
| `--expression <json>` | Match expression as JSON, e.g. {"combinator":"all","children":[...]} | - | No |
| `--action <json>` | Action as JSON, e.g. {"type":"block","status":403} | - | No |
| `--environment-id <id>` | Apply only to this environment | - | No |
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--mode <mode>` | enforce or log-only | `log-only` | No |
| `--disabled` | Create it disabled | - | No |
| `--position <n>` | Insert at this 1-based position (default: last) | - | No |

### `edge-rules update`

Change a rule; only the given parts change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | JSON file with the rule: {name?, expression, action, mode?, enabled?, environment_id?} | - | No |
| `--name <name>` | Rule name (generated from the rule when omitted) | - | No |
| `--expression <json>` | Match expression as JSON, e.g. {"combinator":"all","children":[...]} | - | No |
| `--action <json>` | Action as JSON, e.g. {"type":"block","status":403} | - | No |
| `--environment-id <id>` | Apply only to this environment | - | No |
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--mode <mode>` | enforce or log-only | - | No |
| `--all-environments` | Apply to every environment | - | No |
| `--revision <n>` | Fail if the rule changed since this revision (default: the current one) | - | No |

### `edge-rules delete` (alias: `rm`)

Delete a rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation | - | No |

### `edge-rules move`

Move a rule to another position

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--to <n>` | New 1-based position | - | Yes |

### `edge-rules enable`

Enable a rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules disable`

Disable a rule (kept, never evaluated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules enforce`

Act on matches

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules log-only`

Only record matches, never act on them

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules test`

Dry-run a request against the saved rules (or a rules file) and show the trace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--url <url>` | Request URL or path, e.g. https://app.example.com/admin?x=1 | - | Yes |
| `--method <method>` | HTTP method | `GET` | No |
| `--header <header>` | "Name: value"; repeatable | `` | No |
| `--ip <ip>` | Client address | - | No |
| `--country <code>` | ISO country code, e.g. DE | - | No |
| `--continent <code>` | Continent code, e.g. EU | - | No |
| `--asn <n>` | Autonomous system number | - | No |
| `--user-agent <ua>` | User agent | - | No |
| `--bot-class <class>` | human, search_engine, ai_crawler, ai_agent or unknown_bot | - | No |
| `--environment-id <id>` | Environment the request is for | - | No |
| `--cleared` | The visitor already solved a challenge | - | No |
| `--file <path>` | Evaluate these rules instead of the saved ones (same format as export) | - | No |

### `edge-rules export`

Print the rules as a file apply accepts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--output <path>` | Write to a file instead of stdout | - | No |

### `edge-rules apply`

Make the project rules match a file: rules are matched by name, changed ones updated, missing ones created, others deleted, then ordered as in the file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--file <path>` | Rules file ({"rules": [...]}, as written by export) | - | Yes |
| `--dry-run` | Show the plan without changing anything | - | No |
| `-y, --yes` | Skip confirmation | - | No |
