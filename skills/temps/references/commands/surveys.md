<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `surveys` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `surveys` (alias: `survey`)

Manage in-app surveys (NPS, ratings, choices, open text) and read their answers

**Subcommands:**

- `list` (`ls`) - List surveys
- `get` - Show a survey: its questions and when it is shown
- `create` - Create a survey (a draft unless --status active)
- `update` - Change a survey; only the options given are changed
- `launch` - Start showing a survey to visitors
- `pause` - Stop showing a survey for now
- `complete` - Stop showing a survey for good (its results stay)
- `delete` - Delete a survey (answers already recorded stay in analytics events)
- `results` - Summarise the answers: counts, averages and NPS per question
- `responses` - List individual responses, newest first

### `surveys list` (alias: `ls`)

List surveys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--status <status>` | Only surveys in this status: draft, active, paused, completed | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys get`

Show a survey: its questions and when it is shown

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys create`

Create a survey (a draft unless --status active)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-q, --questions <json|@file|->` | Questions as a JSON array of {id, kind, prompt, optional?, choices?, scale?, lower_label?, upper_label?} | - | Yes |
| `-t, --targeting <json|@file|->` | When to show it: {path_pattern?, trigger_event?, sample_rate?, once_per_visitor?, flag?: {key, value}} | - | No |
| `--description <text>` | Internal description | - | No |
| `--status <status>` | draft (default) or active | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys update`

Change a survey; only the options given are changed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--name <name>` | New name | - | No |
| `--description <text>` | New description ("" clears it) | - | No |
| `-q, --questions <json|@file|->` | Replace the questions | - | No |
| `-t, --targeting <json|@file|->` | Replace when it is shown | - | No |
| `--status <status>` | draft, active, paused or completed | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys launch`

Start showing a survey to visitors

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `surveys pause`

Stop showing a survey for now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `surveys complete`

Stop showing a survey for good (its results stay)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `surveys delete`

Delete a survey (answers already recorded stay in analytics events)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip the confirmation | - | No |

### `surveys results`

Summarise the answers: counts, averages and NPS per question

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Only answers from this environment | - | No |
| `--days <n>` | Look back this many days (default: 30, max: 366) | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys responses`

List individual responses, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Only answers from this environment | - | No |
| `--days <n>` | Look back this many days (default: 30, max: 366) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
