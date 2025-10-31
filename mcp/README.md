# @temps-sdk/mcp

Temps MCP Server - Model Context Protocol integration for the Temps platform.

## Installation

```bash
npm install -g @temps-sdk/mcp
# or
bun add -g @temps-sdk/mcp
```

## Usage

### Running with npx

```bash
npx @temps-sdk/mcp
```

### Using with Claude Desktop or other MCP clients

Add to your MCP client configuration:

```json
{
  "mcpServers": {
    "temps": {
      "command": "npx",
      "args": ["@temps-sdk/mcp"]
    }
  }
}
```

## Available Tools

### get_project_info

Get information about a Temps project.

**Parameters:**
- `project_id` (number, required): The ID of the project

### list_deployments

List deployments for a project.

**Parameters:**
- `project_id` (number, required): The ID of the project
- `limit` (number, optional): Maximum number of deployments to return (default: 10)

## Available Resources

### temps://projects

Returns a list of all Temps projects in JSON format.

## Development

```bash
# Install dependencies
bun install

# Run in development mode
bun run dev

# Build for production
bun run build

# Type check
bun run type-check
```

## License

MIT
