<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `skills` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `skills` (alias: `skill`)

Manage AI skill definitions (global or project-scoped)

**Subcommands:**

- `list` (`ls`) - List skill definitions
- `create` (`add`) - Create a new skill definition. Use @path for content from a file, directory, or tar.gz
- `update` - Update an existing skill definition
- `delete` (`rm`) - Delete a skill definition
- `import` - Import a skill from a public GitHub repository (skills.sh-compatible). Source: <owner>/<repo> or <owner>/<repo>/<skill-name>

### `skills list` (alias: `ls`)

List skill definitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | List global (platform-wide) skills | - | No |
| `--project <slug>` | List skills for a specific project | - | No |
| `--json` | Output in JSON format | - | No |

### `skills create` (alias: `add`)

Create a new skill definition. Use @path for content from a file, directory, or tar.gz

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Skill name | - | Yes |
| `-s, --slug <slug>` | Skill slug (URL-safe identifier) | - | Yes |
| `-c, --content <content>` | Skill content (markdown), @file, @directory, or @archive.tar.gz | - | No |
| `-d, --description <description>` | Skill description | - | No |
| `--global` | Create as global (platform-wide) skill | - | No |
| `--project <slug>` | Create skill for a specific project | - | No |

### `skills update`

Update an existing skill definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New name | - | No |
| `-c, --content <content>` | New content. Prefix with @ to read from file | - | No |
| `-d, --description <description>` | New description | - | No |
| `--global` | Update a global skill | - | No |
| `--project <slug>` | Update a project-scoped skill | - | No |

### `skills delete` (alias: `rm`)

Delete a skill definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | Delete a global skill | - | No |
| `--project <slug>` | Delete a project-scoped skill | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `skills import`

Import a skill from a public GitHub repository (skills.sh-compatible). Source: <owner>/<repo> or <owner>/<repo>/<skill-name>

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-b, --branch <branch>` | Git branch to fetch from | `main` | No |
| `-s, --slug <slug>` | Override slug (defaults to skill directory name) | - | No |
| `-n, --name <name>` | Override skill name (defaults to SKILL.md frontmatter) | - | No |
| `-d, --description <description>` | Override description | - | No |
| `--global` | Install as a global (platform-wide) skill | - | No |
| `--project <slug>` | Install for a specific project | - | No |
| `-f, --force` | Overwrite if a skill with the same slug already exists | - | No |
