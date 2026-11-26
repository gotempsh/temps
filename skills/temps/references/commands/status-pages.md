<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `status-pages` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`status-pages list`](#status-pages-list)
- [`status-pages create`](#status-pages-create)
- [`status-pages show`](#status-pages-show)
- [`status-pages update`](#status-pages-update)
- [`status-pages publish`](#status-pages-publish)
- [`status-pages unpublish`](#status-pages-unpublish)
- [`status-pages rotate-url`](#status-pages-rotate-url)
- [`status-pages delete`](#status-pages-delete)
- [`status-pages domains`](#status-pages-domains)
- [`status-pages subscriptions`](#status-pages-subscriptions)
- [`status-pages subscribers`](#status-pages-subscribers)
- [`status-pages maintenance`](#status-pages-maintenance)
- [`status-pages webhooks`](#status-pages-webhooks)
- [`status-pages password`](#status-pages-password)
- [`status-pages get-overview`](#status-pages-get-overview)
- [`status-pages get-maintenance`](#status-pages-get-maintenance)
- [`status-pages preview`](#status-pages-preview)
- [`status-pages get-public-feed`](#status-pages-get-public-feed)
- [`status-pages get-public-json`](#status-pages-get-public-json)

## `status-pages` (alias: `status-page`)

Publish public status pages showing monitors from any of your projects

**Subcommands:**

- `list` (`ls`) - List status pages, optionally only those showing one project
- `create` - Create a draft status page; monitors may come from any project
- `show` - Show a status page
- `update` - Change the title, description or monitor selection
- `publish` - Publish the page at its public URL
- `unpublish` - Unpublish; the public URL returns 404 immediately
- `rotate-url` - Replace the public URL; the old one stops working
- `delete` (`rm`) - Delete a status page; its public URL returns 404 immediately
- `domains` (`domain`) - Serve a status page on your own hostname
- `subscriptions` - Let visitors subscribe to a status page by email
- `subscribers` - List, export or delete the subscribers of a status page
- `maintenance` - Schedule maintenance windows: shown on the page, no outage alerts, no downtime counted
- `webhooks` - Post every public update of a status page to signed HTTPS webhooks
- `password` - Protect a status page with a password
- `get-overview` - Get status page overview
- `get-maintenance` - A maintenance window
- `preview` - Preview what a status page shows visitors, published or not
- `get-public-feed` - Public status page (Atom feed)
- `get-public-json` - Public status page (JSON)

### `status-pages list` (alias: `ls`)

List status pages, optionally only those showing one project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Only pages with a component from this project | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages create`

Create a draft status page; monitors may come from any project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --title <title>` | Public page title | - | Yes |
| `-m, --monitor <id=name>` | Monitor to show and its public name (repeatable) | - | Yes |
| `-g, --group <name=ids>` | Show monitors together under a group, e.g. "Cloud=12,13" (repeatable) | - | No |
| `-d, --description <text>` | Public description | - | No |
| `--json` | Output in JSON format | - | No |
| `--theme <value>` | Color scheme of the public page (one of: auto, light, dark) | - | No |
| `--accent-color <value>` | Accent color, e.g. #2563eb | - | No |
| `--logo-url <value>` | HTTPS URL of the logo shown in the header | - | No |
| `--website-url <value>` | Where the logo links to | - | No |
| `--show-response-times` | Show monitor response times | - | No |
| `--no-show-response-times` | Set show_response_times to false | - | No |
| `--show-uptime-percentage` | Show uptime percentages | - | No |
| `--no-show-uptime-percentage` | Set show_uptime_percentage to false | - | No |

### `status-pages show`

Show a status page

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages update`

Change the title, description or monitor selection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `-t, --title <title>` | Public page title | - | No |
| `-d, --description <text>` | Public description ("" clears it) | - | No |
| `-m, --monitor <id=name>` | Replace the selection; repeat for each monitor | - | No |
| `-g, --group <name=ids>` | With --monitor: group monitors, e.g. "Cloud=12,13" (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
| `--theme <value>` | Color scheme of the public page (one of: auto, light, dark) | - | No |
| `--accent-color <value>` | Accent color, e.g. #2563eb | - | No |
| `--logo-url <value>` | HTTPS URL of the logo shown in the header | - | No |
| `--website-url <value>` | Where the logo links to | - | No |
| `--show-response-times` | Show monitor response times | - | No |
| `--no-show-response-times` | Set show_response_times to false | - | No |
| `--show-uptime-percentage` | Show uptime percentages | - | No |
| `--no-show-uptime-percentage` | Set show_uptime_percentage to false | - | No |

### `status-pages publish`

Publish the page at its public URL

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages unpublish`

Unpublish; the public URL returns 404 immediately

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages rotate-url`

Replace the public URL; the old one stops working

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages delete` (alias: `rm`)

Delete a status page; its public URL returns 404 immediately

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages domains` (alias: `domain`)

Serve a status page on your own hostname

**Subcommands:**

- `list` (`ls`) - List a status page's custom domains and the DNS records they need
- `add` - Attach a hostname; it serves only after ownership is verified
- `verify` - Prove ownership and issue the certificate
- `renew` - Renew the certificate; the current one keeps serving meanwhile (DNS-01: prints the new TXT record)
- `remove` (`rm`) - Detach a hostname; it answers 404 immediately and its certificate is deleted

#### `status-pages domains list` (alias: `ls`)

List a status page's custom domains and the DNS records they need

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains add`

Attach a hostname; it serves only after ownership is verified

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--hostname <hostname>` | Hostname, e.g. status.example.com | - | Yes |
| `--dns` | Verify with a DNS TXT record instead of HTTP | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains verify`

Prove ownership and issue the certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--domain-id <id>` | Custom domain ID (see `domains list`) | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains renew`

Renew the certificate; the current one keeps serving meanwhile (DNS-01: prints the new TXT record)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--domain-id <id>` | Custom domain ID (see `domains list`) | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains remove` (alias: `rm`)

Detach a hostname; it answers 404 immediately and its certificate is deleted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--domain-id <id>` | Custom domain ID (see `domains list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages subscriptions`

Let visitors subscribe to a status page by email

**Subcommands:**

- `show` - Show whether visitors can subscribe, the sender and what is missing
- `set` - Switch subscriptions on or off, or set the sender address

#### `status-pages subscriptions show`

Show whether visitors can subscribe, the sender and what is missing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages subscriptions set`

Switch subscriptions on or off, or set the sender address

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--enable` | Show the subscribe form on the public page | - | No |
| `--disable` | Hide the subscribe form and stop sending | - | No |
| `--sender <address>` | From address at a verified email domain ("" clears it) | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages subscribers`

List, export or delete the subscribers of a status page

**Subcommands:**

- `list` (`ls`) - List subscribers, newest first
- `export` - Export every subscriber as CSV (email,state,subscribed_at,confirmed_at)
- `delete` (`rm`) - Delete a subscriber; the address gets no more mail

#### `status-pages subscribers list` (alias: `ls`)

List subscribers, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages subscribers export`

Export every subscriber as CSV (email,state,subscribed_at,confirmed_at)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `-o, --output <file>` | Write to a file instead of standard output | - | No |

#### `status-pages subscribers delete` (alias: `rm`)

Delete a subscriber; the address gets no more mail

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--subscriber-id <id>` | Subscriber ID (see `subscribers list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages maintenance`

Schedule maintenance windows: shown on the page, no outage alerts, no downtime counted

**Subcommands:**

- `list` (`ls`) - List maintenance windows, newest first
- `schedule` - Schedule a window; subscribers and webhooks are told once
- `update` - Change a window; once it has started only the title, description and end can change
- `cancel` - Cancel a scheduled window, or end a running one now

#### `status-pages maintenance list` (alias: `ls`)

List maintenance windows, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages maintenance schedule`

Schedule a window; subscribers and webhooks are told once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--title <title>` | Public title (at most 120 characters) | - | Yes |
| `--description <text>` | Public description (at most 2000 characters) | - | No |
| `--starts <when>` | now, or an ISO 8601 date with a zone | - | Yes |
| `--ends <when>` | ISO 8601 date with a zone | - | No |
| `--duration <span>` | Instead of --ends: 90m, 2h, 1d (at most 7d) | - | No |
| `--monitor <id>` | Monitor on this page the window covers (repeat, or comma-separate) | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages maintenance update`

Change a window; once it has started only the title, description and end can change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--maintenance-id <id>` | Maintenance ID (see `maintenance list`) | - | Yes |
| `--title <title>` | Public title | - | No |
| `--description <text>` | Public description ("" removes it) | - | No |
| `--starts <when>` | now, or an ISO 8601 date with a zone | - | No |
| `--ends <when>` | ISO 8601 date with a zone | - | No |
| `--duration <span>` | With --starts, instead of --ends: 90m, 2h, 1d | - | No |
| `--monitor <id>` | Replace the covered monitors (repeat, or comma-separate) | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages maintenance cancel`

Cancel a scheduled window, or end a running one now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--maintenance-id <id>` | Maintenance ID (see `maintenance list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages webhooks`

Post every public update of a status page to signed HTTPS webhooks

**Subcommands:**

- `list` (`ls`) - List the webhooks of a status page and how deliveries are going
- `add` - Add a webhook; its signing secret is printed once
- `update` - Change the URL, or pause and resume deliveries
- `remove` (`rm`) - Delete a webhook; events waiting for it are dropped

#### `status-pages webhooks list` (alias: `ls`)

List the webhooks of a status page and how deliveries are going

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks add`

Add a webhook; its signing secret is printed once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--url <url>` | https:// URL with a public address | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks update`

Change the URL, or pause and resume deliveries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--webhook-id <id>` | Webhook ID (see `webhooks list`) | - | Yes |
| `--url <url>` | New https:// URL | - | No |
| `--enable` | Resume deliveries | - | No |
| `--disable` | Pause deliveries; those still waiting are dropped | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks remove` (alias: `rm`)

Delete a webhook; events waiting for it are dropped

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--webhook-id <id>` | Webhook ID (see `webhooks list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages password`

Protect a status page with a password

**Subcommands:**

- `show` - Show whether a status page is password-protected
- `set` - Set or change the password (prompted, or read from stdin); signs every visitor out

#### `status-pages password show`

Show whether a status page is password-protected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages password set`

Set or change the password (prompted, or read from stdin); signs every visitor out

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--password-stdin` | Read the password from the first line of stdin | - | No |
| `--remove` | Remove the password and open the page again | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages get-overview`

Get status page overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages get-maintenance`

A maintenance window

**Usage:** `status-pages get-maintenance <status-page-id> <maintenance-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `status-pages preview`

Preview what a status page shows visitors, published or not

**Usage:** `status-pages preview <status-page-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `status-pages get-public-feed`

Public status page (Atom feed)

**Usage:** `status-pages get-public-feed <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `status-pages get-public-json`

Public status page (JSON)

**Usage:** `status-pages get-public-json <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
