<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `templates` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `templates` (alias: `tpl`)

Browse deployment templates

**Subcommands:**

- `list` (`ls`) - List available templates
- `validate` - Validate a Temps-native template YAML file or directory offline

### `templates list` (alias: `ls`)

List available templates

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--kind <kind>` | Filter by template gallery (starter, service) | - | No |

### `templates validate`

Validate a Temps-native template YAML file or directory offline

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
