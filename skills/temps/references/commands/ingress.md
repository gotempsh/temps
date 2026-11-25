<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `ingress` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `ingress`

How this Temps machine receives traffic for your apps

**Subcommands:**

- `private` - Serve private addresses from this machine, only to connections from your private network (Tailscale, WireGuard, NetBird, your LAN…)

### `ingress private`

Serve private addresses from this machine, only to connections from your private network (Tailscale, WireGuard, NetBird, your LAN…)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--listen <address>` | One of this machine's private addresses, or "tailscale" for its Tailscale address | - | No |
| `--admit <cidr...>` | Who may connect, as private ranges (default: the network of the --listen address; Tailscale's ranges on Tailscale) | - | No |
| `--tailscale-serve` | Also let Temps write its entries in Tailscale Serve | - | No |
| `--off` | Stop serving private addresses from this machine | - | No |
| `--json` | Output in JSON format | - | No |
