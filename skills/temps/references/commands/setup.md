<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `setup` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `setup`

PoC: install Temps on an existing Linux VPS over SSH and save a client context

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ssh <destination>` | SSH alias or user@hostname (trusted host key required) | - | Yes |
| `--email <email>` | Admin email and certificate contact | - | No |
| `--context <name>` | New local context name | `production` | No |
| `--port <port>` | SSH port | `22` | No |
| `--identity <path>` | SSH private key path (otherwise use your SSH agent/config) | - | No |
| `--channel <channel>` | Runtime release channel: stable, beta, nightly | `stable` | No |
| `--runtime-version <tag>` | Pin the runtime release | - | No |
| `--dry-run` | Show the plan without connecting, changing files or sending telemetry | - | No |
| `--telemetry` | Opt in to coarse setup-step analytics for this attempt only | - | No |
| `--no-telemetry` | Do not send setup analytics (default) | - | No |
| `-y, --yes` | Approve the installation plan without prompting | - | No |
