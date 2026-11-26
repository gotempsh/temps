<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `ai` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`ai chat`](#ai-chat)
- [`ai provider`](#ai-provider)
- [`ai readiness`](#ai-readiness)
- [`ai apps`](#ai-apps)
- [`ai coding-agents`](#ai-coding-agents)
- [`ai gateway`](#ai-gateway)
- [`ai provider-status`](#ai-provider-status)
- [`ai workspace`](#ai-workspace)

## `ai`

AI assistant status for a project

**Subcommands:**

- `chat` - AI chat conversations: messages, attachments, sandboxes and workspaces
- `provider` - Manage the AI CLIs (Claude Code, Codex) Temps runs on this server
- `readiness` - Show which AI prerequisites this project meets, and how to fix the rest
- `apps` - Apps commands
- `coding-agents` - Coding agents commands
- `gateway` - Gateway commands
- `provider-status` - Provider status commands
- `workspace` - Workspace commands

### `ai chat`

AI chat conversations: messages, attachments, sandboxes and workspaces

**Subcommands:**

- `upload` - Attach a file to a conversation in chunks; resumable with --resume
- `create-assistant-conversation` - Start a chat with the Temps assistant
- `list-all-conversations` - List the current user's active conversations across all projects, most-recently-active first, annotated..
- `create-global-conversation` - Create a private user-owned operator thread without binding its lifetime or authority to a project
- `get-user-conversation` - Load one bounded history page for a private conversation by its owner-facing id
- `rename-user-conversation` - Rename user conversation
- `archive-user-conversation` - Archive user conversation
- `start-attachment-upload` - Start uploading a file in chunks, for clients behind a small request-size limit
- `cancel-attachment-upload` - Abandon an upload and free what it reserved
- `append-attachment-chunk` - Append the next chunk of an upload
- `complete-attachment-upload` - Finish an upload once every byte has arrived
- `upload-user-conversation-attachment` - Upload user conversation attachment
- `get-user-conversation-attachment` - Get user conversation attachment
- `compact-conversation` - Summarize the chat's older messages now
- `get-user-conversation-diagnostics` - Export the bounded, server-redacted diagnostic state Temps currently owns
- `list-goals` - List the goals a chat is watching or has finished with, newest first
- `cancel-goal` - Stop watching a goal
- `retry-goal` - Give a failed or waiting goal a fresh attempt budget and check it again now
- `send-user-message` - Submit a turn to a user-owned conversation
- `list-operations` - The deployments, backups and restores this chat started, newest first, with their current state
- `list-user-pending-actions` - List user pending actions
- `update-user-permission-mode` - Update user permission mode
- `resolve-user-permission` - Resolve user permission
- `list-resources` - The projects, databases and services, repositories and sandbox this chat made, with their current state
- `restore-user-conversation` - Restore a user-owned archived conversation
- `unlink-sandbox` - Unlink the chat's sandbox
- `get-sandbox` - Whether the chat can use a sandbox, and which one it is linked to
- `link-sandbox` - Link a sandbox to the chat, or create one for it
- `get-sandbox-changes` - The uncommitted Git changes of every repository under the sandbox work directory
- `get-sandbox-diff` - Both sides of one changed file: as last committed, and as it is now
- `track-sandbox-changes` - Put a folder of the sandbox under Git, so its changes show from now on: a '.gitignore' for dependencies,..
- `read-sandbox-file` - One file of the chat's sandbox, for the viewer
- `list-sandbox-files` - List one directory of the chat's sandbox: folders first, then files
- `connect-sandbox` - Let the chat's sandbox use a project's database
- `disconnect-sandbox` - Stop the chat's sandbox using a project's database
- `connect-sandbox-service` - Let the chat's sandbox use one service, whether or not it belongs to a project
- `disconnect-sandbox-service` - Stop the chat's sandbox using one service
- `wake-sandbox` - Start the chat's sandbox if it went to sleep, so the preview and the assistant's next command find it up
- `list-secret-requests` - List the credentials this chat has asked for and not yet used up or lost
- `provide-secret` - Provide the credential the assistant asked for
- `discard-secret` - Discard a credential request and anything already typed into it
- `stop-user-turn` - Stop user turn
- `get-suggestion-mode` - A chat's next-step suggestion setting, with the default it falls back to
- `update-suggestion-mode` - Set a chat's own next-step suggestion mode, or clear it to follow the user's default
- `suggest-next-steps` - Suggest a few next steps after the chat's latest answer
- `get-user-conversation-usage` - What this conversation has cost so far
- `leave-workspace` - Take this chat out of its workspace
- `get-workspace` - The workspace this chat is in, with its other chats
- `set-workspace` - Put this chat in a workspace, creating it when asked
- `get-user-pending-action` - Get user pending action
- `confirm-user-pending-action` - Confirm user pending action
- `reject-user-pending-action` - Reject user pending action
- `get-suggestion-default` - The user's default for next-step suggestions
- `update-suggestion-default` - Set the user's default for every chat that does not set its own mode
- `find-conversation` - Find the current user's existing chat for a context (returns 'null' if none yet)
- `create-conversation` - Get-or-create the current user's private chat for a context
- `list-conversations` - List the current user's active conversations for a project, most-recently-active first
- `get-conversation` - One bounded conversation-history page (excluding internal context rows)
- `rename-conversation` - Rename a conversation (set its human-facing title)
- `archive-conversation` - Archive (soft-delete) a conversation
- `send-message` - Submit a user message and start a server-owned turn
- `list-pending-actions` - List all pending actions for a conversation (most-recently-proposed first)
- `update-permission-mode` - Update permission mode
- `resolve-permission` - Resolve permission
- `stop-turn` - Explicitly cancel the server-owned active turn
- `get-pending-action` - Get a single pending action by its public id (scoped to the project)
- `confirm-pending-action` - Confirm a proposed AI action: validate permission, atomically claim, execute, persist outcome
- `reject-pending-action` - Reject a proposed AI action (no execution)

#### `ai chat upload`

Attach a file to a conversation in chunks; resumable with --resume

**Usage:** `ai chat upload <conversation-id> <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | File name to show (default: the file name) | - | No |
| `--mime-type <type>` | Content type (default: guessed from the extension) | - | No |
| `--resume <upload-id>` | Continue an interrupted upload instead of starting a new one | - | No |
| `--chunk-bytes <n>` | Bytes per chunk (default 32768, the server's limit) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat create-assistant-conversation`

Start a chat with the Temps assistant

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-model <value>` | Model id to start with; defaults to the provider's default model | - | No |
| `--ai-provider <value>` | An AI-gateway provider id ('gateway_key:<id>') | - | No |
| `--project-id <n>` | A project the chat is about | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-all-conversations`

List the current user's active conversations across all projects, most-recently-active first, annotated..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Conversation lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--scope <value>` | Limit results to the global workspace, or return every readable context (one of: all, global) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of conversations per page (clamped to 1..=100) | - | No |
| `--search <value>` | Keep only chats whose title, or whose project's name or slug, contains this text (case-insensitive,... | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat create-global-conversation`

Create a private user-owned operator thread without binding its lifetime or authority to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-model <value>` | Body field ai_model | - | No |
| `--ai-permission-mode <value>` | Body field ai_permission_mode | - | No |
| `--ai-provider <value>` | A registered development harness | - | No |
| `--ai-thinking-level <value>` | Body field ai_thinking_level | - | No |
| `--kind <value>` | 'ai' (default) or 'terminal' | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation`

Load one bounded history page for a private conversation by its owner-facing id

**Usage:** `ai chat get-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--before <value>` | Opaque next_before cursor returned by the previous page | - | No |
| `--limit <n>` | Messages per page (default 50, maximum 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat rename-user-conversation`

Rename user conversation

**Usage:** `ai chat rename-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--title <value>` | New human-facing title [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat archive-user-conversation`

Archive user conversation

**Usage:** `ai chat archive-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat start-attachment-upload`

Start uploading a file in chunks, for clients behind a small request-size limit

**Usage:** `ai chat start-attachment-upload <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mime-type <value>` | Content type the browser reported, if any | - | No |
| `--name <value>` | File name as the user knows it; unsafe characters are replaced [required] | - | No |
| `--size-bytes <n>` | Size of the whole file in bytes [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat cancel-attachment-upload`

Abandon an upload and free what it reserved

**Usage:** `ai chat cancel-attachment-upload <public-id> <upload-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat append-attachment-chunk`

Append the next chunk of an upload

**Usage:** `ai chat append-attachment-chunk <public-id> <upload-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--offset <n>` | Where this chunk starts in the file: the bytes received so far [required] | - | No |
| `--chunk <path>` | Body field chunk [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat complete-attachment-upload`

Finish an upload once every byte has arrived

**Usage:** `ai chat complete-attachment-upload <public-id> <upload-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat upload-user-conversation-attachment`

Upload user conversation attachment

**Usage:** `ai chat upload-user-conversation-attachment <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | Body field file [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation-attachment`

Get user conversation attachment

**Usage:** `ai chat get-user-conversation-attachment <public-id> <attachment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <value>` | Query parameter name [required] | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat compact-conversation`

Summarize the chat's older messages now

**Usage:** `ai chat compact-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation-diagnostics`

Export the bounded, server-redacted diagnostic state Temps currently owns

**Usage:** `ai chat get-user-conversation-diagnostics <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Persisted rows to include, newest window first (default/max 25) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-goals`

List the goals a chat is watching or has finished with, newest first

**Usage:** `ai chat list-goals <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat cancel-goal`

Stop watching a goal

**Usage:** `ai chat cancel-goal <public-id> <goal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat retry-goal`

Give a failed or waiting goal a fresh attempt budget and check it again now

**Usage:** `ai chat retry-goal <public-id> <goal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat send-user-message`

Submit a turn to a user-owned conversation

**Usage:** `ai chat send-user-message <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-model <value>` | Optional next-turn model, one advertised by the conversation's provider (or by 'ai_provider' when that is set) | - | No |
| `--ai-permission-mode <value>` | Optional next-turn permission mode for the pinned provider harness | - | No |
| `--ai-provider <value>` | Optional AI gateway provider ('gateway_key:<id>') for this and later turns | - | No |
| `--ai-thinking-level <value>` | Optional next-turn thinking level | - | No |
| `--attachments <json>` | Opaque references returned by the conversation attachment endpoint (JSON) | - | No |
| `--content <value>` | Body field content [required] | - | No |
| `--page-context <value>` | Optional, client-supplied description of the page/entity the user is currently viewing (e.g. a trace in a... | - | No |
| `--turn-id <value>` | Client-generated opaque idempotency key for this turn | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-operations`

The deployments, backups and restores this chat started, newest first, with their current state

**Usage:** `ai chat list-operations <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat list-user-pending-actions`

List user pending actions

**Usage:** `ai chat list-user-pending-actions <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat update-user-permission-mode`

Update user permission mode

**Usage:** `ai chat update-user-permission-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--permission-mode <value>` | Provider permission mode to persist [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat resolve-user-permission`

Resolve user permission

**Usage:** `ai chat resolve-user-permission <public-id> <permission-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--decision <json>` | The user's decision for a pending permission request (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-resources`

The projects, databases and services, repositories and sandbox this chat made, with their current state

**Usage:** `ai chat list-resources <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat restore-user-conversation`

Restore a user-owned archived conversation

**Usage:** `ai chat restore-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat unlink-sandbox`

Unlink the chat's sandbox

**Usage:** `ai chat unlink-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-sandbox`

Whether the chat can use a sandbox, and which one it is linked to

**Usage:** `ai chat get-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat link-sandbox`

Link a sandbox to the chat, or create one for it

**Usage:** `ai chat link-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--create` | Create a fresh sandbox for this chat instead | - | No |
| `--no-create` | Set create to false | - | No |
| `--sandbox-id <value>` | Public id of one of the caller's own sandboxes | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-sandbox-changes`

The uncommitted Git changes of every repository under the sandbox work directory

**Usage:** `ai chat get-sandbox-changes <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-sandbox-diff`

Both sides of one changed file: as last committed, and as it is now

**Usage:** `ai chat get-sandbox-diff <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--repo <value>` | The repository root, absolute or relative to the work directory [required] | - | No |
| `--path <value>` | The file, relative to the repository root [required] | - | No |
| `--old-path <value>` | The path before a rename, relative to the repository root | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat track-sandbox-changes`

Put a folder of the sandbox under Git, so its changes show from now on: a '.gitignore' for dependencies,..

**Usage:** `ai chat track-sandbox-changes <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute, or relative to the work directory; the work directory when omitted | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat read-sandbox-file`

One file of the chat's sandbox, for the viewer

**Usage:** `ai chat read-sandbox-file <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute, or relative to the sandbox work directory [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-sandbox-files`

List one directory of the chat's sandbox: folders first, then files

**Usage:** `ai chat list-sandbox-files <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute, or relative to the sandbox work directory | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat connect-sandbox`

Let the chat's sandbox use a project's database

**Usage:** `ai chat connect-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | The project whose services the sandbox may use [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat disconnect-sandbox`

Stop the chat's sandbox using a project's database

**Usage:** `ai chat disconnect-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat connect-sandbox-service`

Let the chat's sandbox use one service, whether or not it belongs to a project

**Usage:** `ai chat connect-sandbox-service <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | A project the service is linked to | - | No |
| `--service-id <n>` | The service the sandbox may use [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat disconnect-sandbox-service`

Stop the chat's sandbox using one service

**Usage:** `ai chat disconnect-sandbox-service <public-id> <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat wake-sandbox`

Start the chat's sandbox if it went to sleep, so the preview and the assistant's next command find it up

**Usage:** `ai chat wake-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat list-secret-requests`

List the credentials this chat has asked for and not yet used up or lost

**Usage:** `ai chat list-secret-requests <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat provide-secret`

Provide the credential the assistant asked for

**Usage:** `ai chat provide-secret <public-id> <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--value-stdin` | The credential [required] (read from stdin) | - | No |
| `--value-file <path>` | The credential [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat discard-secret`

Discard a credential request and anything already typed into it

**Usage:** `ai chat discard-secret <public-id> <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat stop-user-turn`

Stop user turn

**Usage:** `ai chat stop-user-turn <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-suggestion-mode`

A chat's next-step suggestion setting, with the default it falls back to

**Usage:** `ai chat get-suggestion-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat update-suggestion-mode`

Set a chat's own next-step suggestion mode, or clear it to follow the user's default

**Usage:** `ai chat update-suggestion-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | What happens after each answer (one of: off, ask, always) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat suggest-next-steps`

Suggest a few next steps after the chat's latest answer

**Usage:** `ai chat suggest-next-steps <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--answer <value>` | Cursor of the answer the user is looking at, from the message list | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation-usage`

What this conversation has cost so far

**Usage:** `ai chat get-user-conversation-usage <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat leave-workspace`

Take this chat out of its workspace

**Usage:** `ai chat leave-workspace <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-workspace`

The workspace this chat is in, with its other chats

**Usage:** `ai chat get-workspace <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat set-workspace`

Put this chat in a workspace, creating it when asked

**Usage:** `ai chat set-workspace <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--application-id <value>` | Body field application_id | - | No |
| `--create <json>` | A new workspace to create for the chat (JSON) | - | No |
| `--keep-sandbox <value>` | Which sandbox the chats keep when both the chat and the workspace have one (one of: workspace, chat) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-pending-action`

Get user pending action

**Usage:** `ai chat get-user-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat confirm-user-pending-action`

Confirm user pending action

**Usage:** `ai chat confirm-user-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat reject-user-pending-action`

Reject user pending action

**Usage:** `ai chat reject-user-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-suggestion-default`

The user's default for next-step suggestions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat update-suggestion-default`

Set the user's default for every chat that does not set its own mode

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | What happens after each answer (one of: off, ask, always) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat find-conversation`

Find the current user's existing chat for a context (returns 'null' if none yet)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--context-type <value>` | Query parameter context_type [required] | - | No |
| `--context-id <value>` | Query parameter context_id [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat create-conversation`

Get-or-create the current user's private chat for a context

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Body field ai_model | - | No |
| `--ai-permission-mode <value>` | Body field ai_permission_mode | - | No |
| `--ai-provider <value>` | Provider pinned to this conversation | - | No |
| `--ai-thinking-level <value>` | Body field ai_thinking_level | - | No |
| `--context-id <value>` | The entity id (ints stringified) [required] | - | No |
| `--context-type <value>` | e.g. '"deployment"' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-conversations`

List the current user's active conversations for a project, most-recently-active first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-conversation`

One bounded conversation-history page (excluding internal context rows)

**Usage:** `ai chat get-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--before <value>` | Opaque next_before cursor returned by the previous page | - | No |
| `--limit <n>` | Messages per page (default 50, maximum 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat rename-conversation`

Rename a conversation (set its human-facing title)

**Usage:** `ai chat rename-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--title <value>` | New human-facing title [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat archive-conversation`

Archive (soft-delete) a conversation

**Usage:** `ai chat archive-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat send-message`

Submit a user message and start a server-owned turn

**Usage:** `ai chat send-message <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Optional next-turn model, one advertised by the conversation's provider (or by 'ai_provider' when that is set) | - | No |
| `--ai-permission-mode <value>` | Optional next-turn permission mode for the pinned provider harness | - | No |
| `--ai-provider <value>` | Optional AI gateway provider ('gateway_key:<id>') for this and later turns | - | No |
| `--ai-thinking-level <value>` | Optional next-turn thinking level | - | No |
| `--attachments <json>` | Opaque references returned by the conversation attachment endpoint (JSON) | - | No |
| `--content <value>` | Body field content [required] | - | No |
| `--page-context <value>` | Optional, client-supplied description of the page/entity the user is currently viewing (e.g. a trace in a... | - | No |
| `--turn-id <value>` | Client-generated opaque idempotency key for this turn | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-pending-actions`

List all pending actions for a conversation (most-recently-proposed first)

**Usage:** `ai chat list-pending-actions <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat update-permission-mode`

Update permission mode

**Usage:** `ai chat update-permission-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--permission-mode <value>` | Provider permission mode to persist [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat resolve-permission`

Resolve permission

**Usage:** `ai chat resolve-permission <public-id> <permission-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--decision <json>` | The user's decision for a pending permission request (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat stop-turn`

Explicitly cancel the server-owned active turn

**Usage:** `ai chat stop-turn <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-pending-action`

Get a single pending action by its public id (scoped to the project)

**Usage:** `ai chat get-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat confirm-pending-action`

Confirm a proposed AI action: validate permission, atomically claim, execute, persist outcome

**Usage:** `ai chat confirm-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat reject-pending-action`

Reject a proposed AI action (no execution)

**Usage:** `ai chat reject-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `ai provider`

Manage the AI CLIs (Claude Code, Codex) Temps runs on this server

**Subcommands:**

- `login` - Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)

#### `ai provider login`

Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)

**Usage:** `ai provider login <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Print the final sign-in state as JSON | - | No |

### `ai readiness`

Show which AI prerequisites this project meets, and how to fix the rest

**Usage:** `ai readiness [project] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `ai apps`

Apps commands

**Subcommands:**

- `list-applications` - List applications
- `create-application` - Create application
- `archive-application` - Archive an AI application
- `get-application` - Get application
- `rename-application` - Rename an AI application
- `list-application-conversations` - List application conversations
- `create-application-conversation` - Create application conversation
- `list-thread-artifacts` - List thread artifacts
- `create-thread-artifact` - Create thread artifact
- `list-application-git-connections` - List application git connections
- `bind-application-git-connection` - Bind application git connection
- `disconnect-application-git-connection` - Disconnect application git connection
- `create-application-preview-link` - Mint a short-lived URL for a port running in the application harness sandbox
- `create-application-project` - Create and link an application project
- `link-application` - Link an existing project to an application
- `unlink-application` - Unlink an application project
- `deploy-application-workspace` - Deploy an application workspace project with Drop
- `set-application-primary` - Choose the application's primary project
- `write-application-workspace-files` - Write a bounded batch of local files into an application project
- `import-application-workspace-git` - Import a Git repository into an application project
- `restore-application` - Restore an archived AI application
- `get-application-workspace` - Get application workspace
- `update-application-workspace` - Update desired application workspace resources
- `control-application-workspace` - Control an application workspace
- `get-application-workspace-changes` - Get application workspace changes
- `get-application-workspace-diff` - Get application workspace diff
- `get-application-workspace-directory` - Get application workspace directory
- `get-application-workspace-file` - Get application workspace file
- `download-application-workspace-file` - Download one application workspace file
- `upload-application-workspace-files` - Upload files into an application workspace
- `attach-application-workspace-terminal` - Attach an interactive terminal to an application workspace
- `blueprint` - Blueprint commands

#### `ai apps list-applications`

List applications

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Resource lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of applications or conversations per page (clamped to 1..=100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application`

Create application

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--project-ids <n>` | Body field project_ids (repeatable) | - | No |
| `--starter-project <json>` | Body field starter_project (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps archive-application`

Archive an AI application

**Usage:** `ai apps archive-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application`

Get application

**Usage:** `ai apps get-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Resource lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps rename-application`

Rename an AI application

**Usage:** `ai apps rename-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <value>` | New name shown for the application [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps list-application-conversations`

List application conversations

**Usage:** `ai apps list-application-conversations <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Resource lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of applications or conversations per page (clamped to 1..=100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application-conversation`

Create application conversation

**Usage:** `ai apps create-application-conversation <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-provider <value>` | A registered development harness (for example 'claude_cli') | - | No |
| `--kind <value>` | 'ai' (default) or 'terminal' | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps list-thread-artifacts`

List thread artifacts

**Usage:** `ai apps list-thread-artifacts <application-public-id> <conversation-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai apps create-thread-artifact`

Create thread artifact

**Usage:** `ai apps create-thread-artifact <application-public-id> <conversation-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--kind <value>` | Body field kind [required] | - | No |
| `--payload <json>` | Body field payload (JSON) [required] | - | No |
| `--title <value>` | Body field title | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps list-application-git-connections`

List application git connections

**Usage:** `ai apps list-application-git-connections <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai apps bind-application-git-connection`

Bind application git connection

**Usage:** `ai apps bind-application-git-connection <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Body field connectionId [required] | - | No |
| `--project-id <n>` | Body field projectId [required] | - | No |
| `--remote-name <value>` | Body field remoteName [required] | - | No |
| `--repository-id <n>` | Body field repositoryId [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps disconnect-application-git-connection`

Disconnect application git connection

**Usage:** `ai apps disconnect-application-git-connection <application-public-id> <binding-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application-preview-link`

Mint a short-lived URL for a port running in the application harness sandbox

**Usage:** `ai apps create-application-preview-link <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Optional same-origin path to open after the preview grant is exchanged | - | No |
| `--port <n>` | The development server port detected or selected in the sandbox [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application-project`

Create and link an application project

**Usage:** `ai apps create-application-project <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--exposed-port <n>` | Body field exposed_port | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--preset <value>` | Deployment preset for the project | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps link-application`

Link an existing project to an application

**Usage:** `ai apps link-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps unlink-application`

Unlink an application project

**Usage:** `ai apps unlink-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps deploy-application-workspace`

Deploy an application workspace project with Drop

**Usage:** `ai apps deploy-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Target environment | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps set-application-primary`

Choose the application's primary project

**Usage:** `ai apps set-application-primary <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps write-application-workspace-files`

Write a bounded batch of local files into an application project

**Usage:** `ai apps write-application-workspace-files <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--files <json>` | Body field files (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps import-application-workspace-git`

Import a Git repository into an application project

**Usage:** `ai apps import-application-workspace-git <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--depth <n>` | Body field depth | - | No |
| `--git-connection-id <n>` | Opaque user-owned connection reference | - | No |
| `--revision <value>` | Body field revision | - | No |
| `--url <value>` | Body field url [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps restore-application`

Restore an archived AI application

**Usage:** `ai apps restore-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace`

Get application workspace

**Usage:** `ai apps get-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai apps update-application-workspace`

Update desired application workspace resources

**Usage:** `ai apps update-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cpu-limit <number>` | Body field cpu_limit | - | No |
| `--disk-limit-mb <n>` | Body field disk_limit_mb | - | No |
| `--idle-timeout-secs <n>` | Body field idle_timeout_secs | - | No |
| `--image <value>` | Pinned Temps-managed daemon image | - | No |
| `--memory-limit-mb <n>` | Body field memory_limit_mb | - | No |
| `--pids-limit <n>` | Body field pids_limit | - | No |
| `--runtime <value>` | Body field runtime | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps control-application-workspace`

Control an application workspace

**Usage:** `ai apps control-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--action <value>` | restart, pause, resume, rebuild, snapshot, restore, or update_runtime [required] | - | No |
| `--confirm` | Must be true for update_runtime because compute is replaced | - | No |
| `--no-confirm` | Set confirm to false | - | No |
| `--label <value>` | Body field label | - | No |
| `--runtime <value>` | Optional built-in flavor to select during update_runtime | - | No |
| `--snapshot-id <value>` | Body field snapshot_id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-changes`

Get application workspace changes

**Usage:** `ai apps get-application-workspace-changes <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Files per page (1-200, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-diff`

Get application workspace diff

**Usage:** `ai apps get-application-workspace-diff <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-directory`

Get application workspace directory

**Usage:** `ai apps get-application-workspace-directory <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Workspace-relative directory; omit for the root | - | No |
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Entries per page (1-100, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-file`

Get application workspace file

**Usage:** `ai apps get-application-workspace-file <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps download-application-workspace-file`

Download one application workspace file

**Usage:** `ai apps download-application-workspace-file <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps upload-application-workspace-files`

Upload files into an application workspace

**Usage:** `ai apps upload-application-workspace-files <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--files <json>` | Body field files (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps attach-application-workspace-terminal`

Attach an interactive terminal to an application workspace

**Usage:** `ai apps attach-application-workspace-terminal <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--thread <value>` | Public id of the terminal thread to attach to [required] | - | No |
| `--cols <n>` | Initial terminal width in columns (default 80) | - | No |
| `--rows <n>` | Initial terminal height in rows (default 24) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps blueprint`

Blueprint commands

**Subcommands:**

- `get` - The workspace's architecture blueprint with the live state of its parts
- `unbind-component` - Remove a part's binding
- `bind-component` - Bind a part of the accepted blueprint to the project or service that realises it
- `list-versions` - Every version of the workspace's blueprint, newest first
- `save-version` - Save a version the user wrote
- `accept-version` - Accept a proposed version: every chat of the workspace builds against it
- `reject-version` - Reject a proposed version

##### `ai apps blueprint get`

The workspace's architecture blueprint with the live state of its parts

**Usage:** `ai apps blueprint get <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint unbind-component`

Remove a part's binding

**Usage:** `ai apps blueprint unbind-component <public-id> <component-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint bind-component`

Bind a part of the accepted blueprint to the project or service that realises it

**Usage:** `ai apps blueprint bind-component <public-id> <component-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Body field project_id | - | No |
| `--service-id <n>` | Body field service_id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint list-versions`

Every version of the workspace's blueprint, newest first

**Usage:** `ai apps blueprint list-versions <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | 1-based page | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint save-version`

Save a version the user wrote

**Usage:** `ai apps blueprint save-version <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--change-summary <value>` | Body field change_summary [required] | - | No |
| `--document <json>` | The design of an application: what it is for, its parts and its flows (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint accept-version`

Accept a proposed version: every chat of the workspace builds against it

**Usage:** `ai apps blueprint accept-version <public-id> <version> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint reject-version`

Reject a proposed version

**Usage:** `ai apps blueprint reject-version <public-id> <version> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `ai coding-agents`

Coding agents commands

**Subcommands:**

- `list` - Every coding-agent CLI Temps manages, its latest published release, and whether newer releases are..
- `update-settings` - Turn automatic coding-agent updates on or off

#### `ai coding-agents list`

Every coding-agent CLI Temps manages, its latest published release, and whether newer releases are..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--refresh` | Ask the npm registry again instead of using the six-hour cache (at most once a minute) | - | No |
| `--no-refresh` | Set refresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai coding-agents update-settings`

Turn automatic coding-agent updates on or off

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auto-update` | Body field auto_update | - | No |
| `--no-auto-update` | Set auto_update to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `ai gateway`

Gateway commands

**Subcommands:**

- `limits` - Limits commands
- `catalog` - Catalog commands
- `pricing` - Pricing commands
- `providers` - Providers commands
- `usage` - Usage commands
- `create-batch` - Create batch
- `retrieve-batch` - Retrieve batch
- `cancel-batch` - Cancel batch
- `chat-completions` - Chat completions
- `embeddings` - Embeddings
- `upload-file` - Upload file
- `delete-file` - Delete file
- `retrieve-file` - Retrieve file
- `file-content` - File content
- `list-models` - List models
- `create-response-json` - Explicit JSON contract for generated clients; the standard OpenAI route remains polymorphic
- `create-response-stream` - Explicit SSE contract for generated clients; never buffers a successful event stream
- `stream-structured-output` - Stream structured output

#### `ai gateway limits`

Limits commands

**Subcommands:**

- `get-overview` - Get limits overview
- `update-instance` - Update instance limits
- `reset` - Reset project limits
- `get` - Get project limits
- `update` - Update project limits

##### `ai gateway limits get-overview`

Get limits overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page of project overrides (default 1) | - | No |
| `--page-size <n>` | Page size (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits update-instance`

Update instance limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--max-cost-per-month-microcents <n>` | Operator-funded spend per UTC calendar month, in microcents (1/10000 of a cent; 1 USD = 1,000,000 microcents) | - | No |
| `--max-requests-per-minute <n>` | Requests per rolling minute | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits reset`

Reset project limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits get`

Get project limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits update`

Update project limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--max-cost-per-month-microcents <n>` | Operator-funded spend per UTC calendar month, in microcents (1/10000 of a cent; 1 USD = 1,000,000 microcents) | - | No |
| `--max-requests-per-minute <n>` | Requests per rolling minute | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway catalog`

Catalog commands

**Subcommands:**

- `get-model` - The runtime model catalog's settings and the outcome of its last refresh
- `list-model-entries` - The stored catalog's models, newest release first
- `refresh-model` - Fetch the catalog now, store it and apply its prices
- `update-model-settings` - Change where the catalog comes from and how it is refreshed
- `list-unpriced-models` - Models enabled on a provider key that no price covers

##### `ai gateway catalog get-model`

The runtime model catalog's settings and the outcome of its last refresh

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog list-model-entries`

The stored catalog's models, newest release first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--provider <value>` | Temps provider id ('openai', 'anthropic', 'gemini', 'xai', 'openrouter') | - | No |
| `--search <value>` | Case-insensitive part of the model id or name | - | No |
| `--page <n>` | Page number, from 1 (default 1) | - | No |
| `--page-size <n>` | Page size (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog refresh-model`

Fetch the catalog now, store it and apply its prices

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog update-model-settings`

Change where the catalog comes from and how it is refreshed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auto-refresh` | Body field auto_refresh | - | No |
| `--no-auto-refresh` | Set auto_refresh to false | - | No |
| `--discover-provider-models` | Body field discover_provider_models | - | No |
| `--no-discover-provider-models` | Set discover_provider_models to false | - | No |
| `--refresh-interval-hours <n>` | Body field refresh_interval_hours | - | No |
| `--source-url <value>` | A custom 'https://' catalog URL, or an empty string to go back to the catalog published in the Temps... | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog list-unpriced-models`

Models enabled on a provider key that no price covers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway pricing`

Pricing commands

**Subcommands:**

- `get` - Get pricing

##### `ai gateway pricing get`

Get pricing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway providers`

Providers commands

**Subcommands:**

- `list-keys` - List provider keys
- `create-key` - Create provider key
- `discover-models` - Discover provider models
- `test-key-inline` - Test provider key inline
- `delete-key` - Delete provider key
- `get-key` - Get provider key
- `update-key` - Update provider key
- `add-model` - Add provider model
- `refresh-models` - Refresh provider models
- `delete-model` - Delete provider model
- `update-model` - Update provider model
- `test-key-by-id` - Test provider key by id

##### `ai gateway providers list-keys`

List provider keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers create-key`

Create provider key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | Provider API key (read from stdin) | - | No |
| `--api-key-file <path>` | Provider API key (read from a file) | - | No |
| `--base-url <value>` | Required for "openai_compatible", e.g. "http://10.0.0.5:8000/v1" | - | No |
| `--capabilities <json>` | Capabilities an administrator declared for a self-hosted key (JSON) | - | No |
| `--default-model <value>` | Model id to pin for this provider (e.g. "gpt-6.2-sol") | - | No |
| `--display-name <value>` | Body field display_name [required] | - | No |
| `--provider <value>` | "openai", "anthropic", "xai", "gemini", "openrouter", or "openai_compatible" for a self-hosted... [required] | - | No |
| `--trust-private-address` | Trust the exact private 'ip:port' of 'base_url' for this key only, without adding it to the instance-wide... | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers discover-models`

Discover provider models

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | API key, if the server requires one (read from stdin) | - | No |
| `--api-key-file <path>` | API key, if the server requires one (read from a file) | - | No |
| `--base-url <value>` | Base URL of the server (required for "openai_compatible") | - | No |
| `--provider <value>` | Provider ID, e.g. "openai_compatible" [required] | - | No |
| `--trust-private-address` | Discover as if the base URL's exact private address were trusted on the key | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers test-key-inline`

Test provider key inline

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | The raw API key to test (may be empty for a self-hosted provider) (read from stdin) | - | No |
| `--api-key-file <path>` | The raw API key to test (may be empty for a self-hosted provider) (read from a file) | - | No |
| `--base-url <value>` | Optional custom base URL (required for "openai_compatible") | - | No |
| `--default-model <value>` | Model to send the test completion to | - | No |
| `--provider <value>` | Provider ID: "openai", "anthropic", "xai", "gemini", "openrouter", "openai_compatible" [required] | - | No |
| `--trust-private-address` | Test as if the base URL's exact private address were trusted on the key | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers delete-key`

Delete provider key

**Usage:** `ai gateway providers delete-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers get-key`

Get provider key

**Usage:** `ai gateway providers get-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers update-key`

Update provider key

**Usage:** `ai gateway providers update-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | A credential (read from stdin) | - | No |
| `--api-key-file <path>` | A credential (read from a file) | - | No |
| `--base-url <value>` | Double-Option: absent = leave unchanged, present-null = clear (revert to the provider's default endpoint),... | - | No |
| `--capabilities <json>` | Capabilities an administrator declared for a self-hosted key (JSON) | - | No |
| `--default-model <value>` | Double-Option: absent = leave unchanged, present-null = clear the pinned model (revert to the per-provider... | - | No |
| `--display-name <value>` | Body field display_name | - | No |
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--trust-private-address` | 'true' trusts the exact private 'ip:port' of the (new) base URL for this key only (requires... | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers add-model`

Add provider model

**Usage:** `ai gateway providers add-model <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--display-name <value>` | Body field display_name | - | No |
| `--model-id <value>` | Body field model_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers refresh-models`

Refresh provider models

**Usage:** `ai gateway providers refresh-models <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers delete-model`

Delete provider model

**Usage:** `ai gateway providers delete-model <id> <model-row-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers update-model`

Update provider model

**Usage:** `ai gateway providers update-model <id> <model-row-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--is-enabled` | Body field is_enabled [required] | - | No |
| `--no-is-enabled` | Set is_enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers test-key-by-id`

Test provider key by id

**Usage:** `ai gateway providers test-key-by-id <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway usage`

Usage commands

**Subcommands:**

- `get-by` - Get usage by project
- `get-by-provider` - Get usage by provider
- `get-conversations` - Get conversations
- `get-conversation-detail` - Get conversation detail
- `get-recent` - Get usage recent
- `get-summary` - Get usage summary
- `get-timeseries` - Get usage timeseries
- `get-top-models` - Get usage top models

##### `ai gateway usage get-by`

Get usage by project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--limit <n>` | Max projects (defaults to 50, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-by-provider`

Get usage by provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-conversations`

Get conversations

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--limit <n>` | Max results (defaults to 50, max 100) | - | No |
| `--user-id <n>` | Filter by user ID | - | No |
| `--tags <value>` | Filter by tags (comma-separated) | - | No |
| `--model <value>` | Filter by model name | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-conversation-detail`

Get conversation detail

**Usage:** `ai gateway usage get-conversation-detail <conversation-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max results (defaults to 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-recent`

Get usage recent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Page size (defaults to 20, max 50) | - | No |
| `--offset <n>` | Number of results to skip for pagination (defaults to 0) | - | No |
| `--provider <value>` | Filter by provider name | - | No |
| `--model <value>` | Filter by model name | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--status <n>` | Filter by HTTP status code (exact match) | - | No |
| `--cost-gte <n>` | Cost greater-than-or-equal, in microcents | - | No |
| `--cost-gt <n>` | Cost strictly greater-than, in microcents | - | No |
| `--cost-lte <n>` | Cost less-than-or-equal, in microcents | - | No |
| `--cost-lt <n>` | Cost strictly less-than, in microcents | - | No |
| `--tokens-gte <n>` | Total tokens greater-than-or-equal | - | No |
| `--tokens-gt <n>` | Total tokens strictly greater-than | - | No |
| `--tokens-lte <n>` | Total tokens less-than-or-equal | - | No |
| `--tokens-lt <n>` | Total tokens strictly less-than | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-summary`

Get usage summary

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-timeseries`

Get usage timeseries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--bucket <value>` | Bucket size: hour, day, week (defaults to day) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-top-models`

Get usage top models

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--limit <n>` | Max results (defaults to 10) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway create-batch`

Create batch

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--completion-window <value>` | Currently only '24h' [required] | - | No |
| `--endpoint <value>` | Endpoint every request in the input file targets: '/v1/responses', '/v1/chat/completions' or '/v1/embeddings' [required] | - | No |
| `--input-file-id <value>` | A file uploaded through 'POST /ai/v1/files' by the same caller [required] | - | No |
| `--metadata <json>` | Body field metadata (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway retrieve-batch`

Retrieve batch

**Usage:** `ai gateway retrieve-batch <batch-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway cancel-batch`

Cancel batch

**Usage:** `ai gateway cancel-batch <batch-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway chat-completions`

Chat completions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--frequency-penalty <number>` | Body field frequency_penalty | - | No |
| `--max-tokens <n>` | Body field max_tokens | - | No |
| `--messages <json>` | Body field messages (JSON) [required] | - | No |
| `--model <value>` | Body field model [required] | - | No |
| `--n <n>` | Body field n | - | No |
| `--presence-penalty <number>` | Body field presence_penalty | - | No |
| `--response-format <json>` | Body field response_format (JSON) | - | No |
| `--seed <n>` | Body field seed | - | No |
| `--stop <json>` | Body field stop (JSON) | - | No |
| `--stream` | Body field stream | - | No |
| `--no-stream` | Set stream to false | - | No |
| `--temperature <number>` | Body field temperature | - | No |
| `--tool-choice <json>` | Body field tool_choice (JSON) | - | No |
| `--tools <json>` | Body field tools (JSON) | - | No |
| `--top-p <number>` | Body field top_p | - | No |
| `--user <value>` | Body field user | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway embeddings`

Embeddings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--dimensions <n>` | Body field dimensions | - | No |
| `--encoding-format <value>` | Body field encoding_format | - | No |
| `--body-input <json>` | Body field input (JSON) [required] | - | No |
| `--model <value>` | Body field model [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway upload-file`

Upload file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | A JSONL batch input file: one request per line, every line targeting the same model and endpoint [required] | - | No |
| `--purpose <value>` | Must be 'batch' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway delete-file`

Delete file

**Usage:** `ai gateway delete-file <file-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway retrieve-file`

Retrieve file

**Usage:** `ai gateway retrieve-file <file-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway file-content`

File content

**Usage:** `ai gateway file-content <file-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway list-models`

List models

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway create-response-json`

Explicit JSON contract for generated clients; the standard OpenAI route remains polymorphic

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--background` | Run asynchronously and poll for the result | - | No |
| `--no-background` | Set background to false | - | No |
| `--conversation <json>` | Attach the response to a stored conversation (JSON) | - | No |
| `--model <value>` | Model to run, e.g. 'gpt-6-luna' [required] | - | No |
| `--previous-response-id <value>` | Continue from a stored response | - | No |
| `--stream` | Stream the reply as server-sent events | - | No |
| `--no-stream` | Set stream to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway create-response-stream`

Explicit SSE contract for generated clients; never buffers a successful event stream

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--background` | Run asynchronously and poll for the result | - | No |
| `--no-background` | Set background to false | - | No |
| `--conversation <json>` | Attach the response to a stored conversation (JSON) | - | No |
| `--model <value>` | Model to run, e.g. 'gpt-6-luna' [required] | - | No |
| `--previous-response-id <value>` | Continue from a stored response | - | No |
| `--stream` | Stream the reply as server-sent events | - | No |
| `--no-stream` | Set stream to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

#### `ai gateway stream-structured-output`

Stream structured output

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--cache-key <value>` | Optional screen-owned cache identity | - | No |
| `--cache-ttl-seconds <n>` | Backend cache lifetime in seconds (default 60, clamped to 1-300) | - | No |
| `--model <value>` | Optional enabled model id for the selected key | - | No |
| `--prompt <value>` | The complete, already-authorized context and instructions for the model [required] | - | No |
| `--provider-key-id <n>` | Optional configured gateway key | - | No |
| `--purpose <value>` | Stable product use-case identifier, for example 'api_traffic.summary' [required] | - | No |
| `--refresh` | Bypass any matching backend cache entry and replace it with this result | - | No |
| `--no-refresh` | Set refresh to false | - | No |
| `--schema <json>` | JSON Schema describing the required response object (JSON) [required] | - | No |
| `--thinking-level <value>` | Optional normalized reasoning depth advertised for the selected model | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

### `ai provider-status`

Provider status commands

**Subcommands:**

- `get` - Get ai provider status
- `refresh` - Refresh ai provider status
- `update-summary-preference` - Update ai summary preference

#### `ai provider-status get`

Get ai provider status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai provider-status refresh`

Refresh ai provider status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai provider-status update-summary-preference`

Update ai summary preference

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--model <value>` | Body field model | - | No |
| `--provider-id <value>` | Body field provider_id | - | No |
| `--thinking-level <value>` | Body field thinking_level | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `ai workspace`

Workspace commands

**Subcommands:**

- `get-global` - Get global ai workspace
- `get-activity` - Aggregate all active threads for the caller's global workspace and up to 100 requested, visible..
- `get-global-changes` - Get global workspace changes
- `get-global-diff` - Get global workspace diff
- `get-global-directory` - Get global workspace directory
- `get-global-file` - Get global workspace file
- `get-file-limits` - Get effective AI workspace file limits
- `download-global-file` - Download one global AI workspace file
- `upload-global-files` - Upload files into the global AI workspace
- `create-global-preview-link` - Mint a short-lived preview URL for a port in the user's shared global AI workspace
- `attach-global-terminal` - Attach an interactive terminal to the user's global workspace

#### `ai workspace get-global`

Get global ai workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-activity`

Aggregate all active threads for the caller's global workspace and up to 100 requested, visible..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--application-public-ids <value>` | Comma-separated application public IDs (maximum 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-changes`

Get global workspace changes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Files per page (1-200, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-diff`

Get global workspace diff

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-directory`

Get global workspace directory

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Workspace-relative directory; omit for the root | - | No |
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Entries per page (1-100, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-file`

Get global workspace file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-file-limits`

Get effective AI workspace file limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai workspace download-global-file`

Download one global AI workspace file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace upload-global-files`

Upload files into the global AI workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--files <json>` | Body field files (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace create-global-preview-link`

Mint a short-lived preview URL for a port in the user's shared global AI workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Optional same-origin path to open after the preview grant is exchanged | - | No |
| `--port <n>` | The development server port detected or selected in the sandbox [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace attach-global-terminal`

Attach an interactive terminal to the user's global workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--thread <value>` | Public id of the terminal thread to attach to [required] | - | No |
| `--cols <n>` | Initial terminal width in columns (default 80) | - | No |
| `--rows <n>` | Initial terminal height in rows (default 24) | - | No |
| `--json` | Output in JSON format | - | No |
