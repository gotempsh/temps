<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `login` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `login`

Authenticate with a Temps server. Opens the browser for interactive logins; use --no-wait then --resume when a person approves but the caller cannot keep a command open, and --api-key for headless / CI.

**Usage:** `login [url] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --api-key <key>` | Use a pre-minted API key (Settings → API Keys) instead of opening the browser. Required for headless / CI. | - | No |
| `--context <name>` | Save the credentials under this context name (defaults to URL host) | - | No |
| `--no-wait` | Print the browser approval link and exit instead of waiting for it. For agents and scripts whose shell returns output only when a command exits; finish with --resume. | - | No |
| `--resume` | Finish a sign-in started with --no-wait: wait for the browser approval, then save the credentials. Exits with code 2 if it is not approved yet; run it again. | - | No |
| `--wait-secs <seconds>` | With --resume: how long to wait for the approval before exiting with code 2 (default: 90) | - | No |
| `--debug` | Print every request/response (URL, status, headers, raw body) to stderr. Also enabled via TEMPS_DEBUG=1. | - | No |
| `--api-key-file <path>` | Read the API key from a file (- reads stdin) instead of --api-key | - | No |
| `--api-key-stdin` | Read the API key from stdin instead of --api-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |
