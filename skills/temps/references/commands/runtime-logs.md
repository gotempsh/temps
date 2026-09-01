<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `runtime-logs` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `runtime-logs` (alias: `rlogs`)

View runtime container logs (use -f to follow in real-time)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name | `production` | No |
| `-c, --container <id>` | Container ID (partial match supported) | - | No |
| `-d, --deployment <id>` | Deployment ID, including failed retained containers | - | No |
| `-n, --tail <lines>` | Number of lines to tail | `1000` | No |
| `-t, --timestamps` | Show timestamps | - | No |
| `-f, --follow` | Follow log output (stream in real-time) | - | No |
