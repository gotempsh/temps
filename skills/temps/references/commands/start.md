<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `start` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `start`

Run Temps on this computer, sign in the CLI and add the Temps skill for your coding agent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--channel <channel>` | Release channel for a fresh install: stable, beta or nightly | `beta` | No |
| `--runtime-version <tag>` | Run this exact Temps release | - | No |
| `--skills <scope>` | Where to add the Temps agent skill: project, global or none (default: project inside a project, otherwise global) | - | No |
| `--port <port>` | Port for your apps and the console on a fresh install (default: 8080 or the next free port) | - | No |
| `--make-default` | Make "local" the default CLI context even if another one is active | - | No |
| `--json` | Print the result as JSON, for scripts and agents | - | No |
