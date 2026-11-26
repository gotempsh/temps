<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `preview-gateway` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `preview-gateway` (alias: `pgw`)

Manage the preview gateway that serves workspace preview URLs (status, settings, logs, restart, upgrade)

**Subcommands:**

- `status` - Show the gateway container's health, image and last setup error
- `settings` (`show`) - Show the gateway settings: enabled, image, host port, auto-upgrade, container name, and their defaults
- `configure` (`set`) - Change gateway settings; only the flags you pass are changed
- `logs` - Print the latest gateway container log lines
- `restart` - Restart the gateway container (previews are briefly unavailable)
- `upgrade` - Pull an image and recreate the gateway with it (previews are briefly unavailable)

### `preview-gateway status`

Show the gateway container's health, image and last setup error

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `preview-gateway settings` (alias: `show`)

Show the gateway settings: enabled, image, host port, auto-upgrade, container name, and their defaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `preview-gateway configure` (alias: `set`)

Change gateway settings; only the flags you pass are changed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Run the gateway (creates its containers) | - | No |
| `--disable` | Stop running the gateway (removes its containers; preview URLs stop being served) | - | No |
| `--image <ref>` | Gateway image to run, ideally an immutable @sha256 reference | - | No |
| `--reset-image` | Go back to the image pinned in this Temps build | - | No |
| `--host-port <port>` | Host port the ingress relay's :8080 is published on | - | No |
| `--auto-upgrade` | Apply the pinned image on every Temps startup (default) | - | No |
| `--no-auto-upgrade` | Leave the running image alone on startup; upgrade by hand | - | No |
| `--container-name <name>` | Docker container name; change only when several Temps instances share one Docker daemon | - | No |
| `--reset-container-name` | Go back to the default container name | - | No |
| `-y, --yes` | Do not ask before a change that interrupts previews (--disable, a new container name) | - | No |
| `--json` | Print the saved settings as JSON | - | No |

### `preview-gateway logs`

Print the latest gateway container log lines

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tail <lines>` | Lines to print, 1 to 2000 (default 200) | - | No |
| `--json` | Output in JSON format | - | No |

### `preview-gateway restart`

Restart the gateway container (previews are briefly unavailable)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |

### `preview-gateway upgrade`

Pull an image and recreate the gateway with it (previews are briefly unavailable)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Image to pull and run, ideally an immutable @sha256 reference | - | No |
| `--default` | Use the image pinned in this Temps build | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
