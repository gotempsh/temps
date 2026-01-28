---
name: temps-mcp-setup
description: |
  Configure the Temps MCP server to enable AI assistants to interact with the Temps platform. Provides tools for listing projects, viewing project details, and managing deployments directly from Claude or other MCP-compatible clients. Use when the user wants to: (1) Set up Temps MCP server, (2) Configure Claude to manage Temps projects, (3) Add Temps tools to their AI assistant, (4) Enable AI-powered deployment management, (5) Connect Claude Desktop to Temps, (6) Use MCP to interact with Temps API. Triggers: "temps mcp", "configure temps tools", "add temps to claude", "temps ai assistant", "mcp server setup".
---

# Temps MCP Setup

Configure the Temps MCP server to manage projects and deployments from AI assistants.

## Installation

### Option 1: npx (Recommended)

No installation needed - runs directly:

```json
{
  "mcpServers": {
    "temps": {
      "command": "npx",
      "args": ["-y", "@temps-sdk/mcp"],
      "env": {
        "TEMPS_API_URL": "https://your-temps-instance.com",
        "TEMPS_API_KEY": "your-api-key"
      }
    }
  }
}
```

### Option 2: Global Install

```bash
npm install -g @temps-sdk/mcp
```

## Configuration by Client

### Claude Desktop (macOS)

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "temps": {
      "command": "npx",
      "args": ["-y", "@temps-sdk/mcp"],
      "env": {
        "TEMPS_API_URL": "https://your-temps-instance.com",
        "TEMPS_API_KEY": "your-api-key"
      }
    }
  }
}
```

### Claude Desktop (Windows)

Edit `%APPDATA%\Claude\claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "temps": {
      "command": "npx",
      "args": ["-y", "@temps-sdk/mcp"],
      "env": {
        "TEMPS_API_URL": "https://your-temps-instance.com",
        "TEMPS_API_KEY": "your-api-key"
      }
    }
  }
}
```

### Claude Code (VS Code)

Add to `.vscode/settings.json` or user settings:

```json
{
  "claude.mcpServers": {
    "temps": {
      "command": "npx",
      "args": ["-y", "@temps-sdk/mcp"],
      "env": {
        "TEMPS_API_URL": "https://your-temps-instance.com",
        "TEMPS_API_KEY": "your-api-key"
      }
    }
  }
}
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `TEMPS_API_URL` | Yes | Your Temps instance URL |
| `TEMPS_API_KEY` | Yes | API key from Temps dashboard |

### Getting Your API Key

1. Log into your Temps dashboard
2. Navigate to Settings > API Keys
3. Create a new API key with appropriate permissions
4. Copy the key (it's only shown once)

## Available Tools

Once configured, these tools become available:

### list_projects

List all projects in your Temps instance.

```
Parameters:
- page (optional): Page number, default 1
- page_size (optional): Items per page, default 20, max 100
```

### get_project

Get details of a specific project.

```
Parameters:
- project_id (required): The project ID
```

### list_deployments

List deployments for a project.

```
Parameters:
- project_id (required): The project ID
- page (optional): Page number, default 1
- page_size (optional): Items per page, default 20, max 100
```

## Available Prompts

### add_react_analytics

Guided setup for adding Temps analytics to React applications.

```
Arguments:
- framework (required): nextjs-app, nextjs-pages, vite, cra, remix
- project_id (optional): Your Temps project ID
```

## Verification

After configuration, restart your client and verify:

1. Ask: "List my Temps projects"
2. The assistant should use `list_projects` tool
3. You should see your projects listed

## Troubleshooting

**Tools not appearing?**
- Restart your MCP client completely
- Verify JSON syntax is valid
- Check that npx is in your PATH

**Connection errors?**
- Verify TEMPS_API_URL is accessible
- Check API key has correct permissions
- Try accessing the URL in a browser

**Permission denied?**
- Ensure API key has read permissions for projects
- Check API key hasn't expired
