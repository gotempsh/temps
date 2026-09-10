<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `cluster` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `cluster`

Cluster-wide multi-node operations

**Subcommands:**

- `dns` - Cluster DNS resolver (ADR-024) operations

### `cluster dns`

Cluster DNS resolver (ADR-024) operations

**Subcommands:**

- `status` - Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs

#### `cluster dns status`

Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
