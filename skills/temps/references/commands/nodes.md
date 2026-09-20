<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `nodes` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `nodes`

Worker nodes and workload placement

**Subcommands:**

- `capability` - Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued

### `nodes capability`

Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
