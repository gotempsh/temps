---
name: temps-mcp-setup
description: |
  Configure the Temps MCP server to enable AI assistants to interact with the Temps platform. Provides tools for listing projects, viewing project details, and managing deployments directly from Claude or other MCP-compatible clients. Use when the user wants to: (1) Set up Temps MCP server, (2) Configure Claude to manage Temps projects, (3) Add Temps tools to their AI assistant, (4) Enable AI-powered deployment management, (5) Connect Claude Desktop to Temps, (6) Use MCP to interact with Temps API. Triggers: "temps mcp", "configure temps tools", "add temps to claude", "temps ai assistant", "mcp server setup".
---

# Temps MCP Setup

Configure the Temps MCP server to manage projects and deployments from AI assistants.

## Installation

**npx (recommended)** — no install needed. Or install globally: `npm install -g @temps-sdk/mcp`

## Configuration by Client

All clients use the same MCP server block — only the config file location and JSON key differ:

| Client | Config file | Root key |
|--------|------------|----------|
| Claude Desktop (macOS) | `~/Library/Application Support/Claude/claude_desktop_config.json` | `mcpServers` |
| Claude Desktop (Windows) | `%APPDATA%\Claude\claude_desktop_config.json` | `mcpServers` |
| Claude Code (VS Code) | `.vscode/settings.json` or user settings | `claude.mcpServers` |

**Server block** (nest under the appropriate root key):

```json
"temps": {
  "command": "npx",
  "args": ["-y", "@temps-sdk/mcp"],
  "env": {
    "TEMPS_API_URL": "https://your-temps-instance.com",
    "TEMPS_API_KEY": "your-api-key"
  }
}
```

### Getting Your API Key

1. Log into Temps dashboard → Settings → API Keys
2. Create a new key with appropriate permissions
3. Copy immediately (shown once only)

## Available Tools

| Tool | Required params | Optional params | Description |
|------|----------------|-----------------|-------------|
| `list_projects` | — | `page` (default 1), `page_size` (default 20, max 100) | List all projects |
| `get_project` | `project_id` | — | Get project details |
| `list_deployments` | `project_id` | `page` (default 1), `page_size` (default 20, max 100) | List project deployments |

## Available Prompts

| Prompt | Required args | Optional args | Description |
|--------|--------------|---------------|-------------|
| `add_react_analytics` | `framework` (nextjs-app, nextjs-pages, vite, cra, remix) | `project_id` | Guided React analytics setup |

## Verification

After configuration, restart your client, then ask: "List my Temps projects" — the assistant should invoke `list_projects` and return results.

## Troubleshooting

| Symptom | Checks |
|---------|--------|
| Tools not appearing | Restart MCP client; verify JSON syntax; confirm `npx` is in PATH |
| Connection errors | Verify `TEMPS_API_URL` is reachable; check API key permissions |
| Permission denied | Ensure API key has read permissions; check key hasn't expired |
