<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `logout` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `logout`

Revoke the active context's API key on the server and forget it locally

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--context <name>` | Log out of a specific context (defaults to active) | - | No |
| `--local-only` | Skip server-side revocation; only clear local credentials | - | No |
