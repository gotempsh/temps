<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `login` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `login`

Authenticate with a Temps server. Opens the browser for interactive logins; use --api-key for headless / CI.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --api-key <key>` | Use a pre-minted API key (Settings → API Keys) instead of opening the browser. Required for headless / CI. | - | No |
| `--context <name>` | Save the credentials under this context name (defaults to URL host) | - | No |
| `--debug` | Print every request/response (URL, status, headers, raw body) to stderr. Also enabled via TEMPS_DEBUG=1. | - | No |
