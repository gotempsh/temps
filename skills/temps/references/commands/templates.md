<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `templates` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `templates` (alias: `tpl`)

Browse templates and create them

**Subcommands:**

- `list` (`ls`) - List available templates
- `create` - Create a template: every service (an app, what `projects` manages), the databases it needs, its shared variables and path routes
- `validate` - Validate a Temps-native template YAML file or directory offline

### `templates list` (alias: `ls`)

List available templates

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--kind <kind>` | Filter by kind: starter or app | - | No |

### `templates create`

Create a template: every service (an app, what `projects` manages), the databases it needs, its shared variables and path routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <name>` | Put the services in a new project (group) with this name; the default for several services | - | No |
| `--into <project>` | Add the services to an existing project (group), by slug or id (see: project-groups list) | - | No |
| `--standalone` | Create a one-service template without a project (group); the default for one service | - | No |
| `--name <service=name...>` | Name a created service; a one-service template also takes a bare name | - | No |
| `--env <[service:]KEY=VALUE...>` | Set a variable; prefix the service when the template cannot tell | - | No |
| `--secret <[service:]KEY=VALUE...>` | Set a secret variable (write-only once stored) | - | No |
| `--database <type=new|type=new:name|type=id...>` | Create or link each database the template needs (default: create) | - | No |
| `--fork-connection <id>` | Starters only: copy the code into a new repository on this git connection | - | No |
| `--repo-name <name>` | Name of the new repository (with --fork-connection) | - | No |
| `--repo-owner <owner>` | Account or organization for the new repository | - | No |
| `--public-repo` | Make the new repository public (private by default) | - | No |
| `--no-auto-deploy` | Do not deploy on push to the new repository | - | No |
| `--idempotency-key <key>` | Reuse a key to get the same result instead of creating twice | - | No |
| `--no-wait` | Return right away instead of waiting until it finishes | - | No |
| `--timeout <minutes>` | Stop waiting after this long (default 30); it keeps running on the server | - | No |
| `--json` | Output in JSON format | - | No |

### `templates validate`

Validate a Temps-native template YAML file or directory offline

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
