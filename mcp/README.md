# Temps MCP Server

Model Context Protocol (MCP) server for the Temps platform. Provides AI assistants with reusable prompts to interact with Temps projects, deployments, and infrastructure.

## Features

- **Prompts**: Reusable prompt templates for common Temps operations
  - `add_react_analytics` - Step-by-step guide to add analytics to React apps

## Installation

```bash
# Install dependencies
bun install

# Build the server
bun run build
```

## Quick Start

### Test with MCP Inspector (Recommended)

```bash
# Install MCP Inspector globally
npm install -g @modelcontextprotocol/inspector

# Run the inspector
mcp-inspector node dist/index.js
```

This opens a web interface where you can test all prompts interactively.

### Use with Claude Desktop

Add to your Claude Desktop configuration file:

**macOS/Linux**: `~/Library/Application Support/Claude/claude_desktop_config.json`

**Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "temps": {
      "command": "node",
      "args": ["/absolute/path/to/temps/mcp/dist/index.js"]
    }
  }
}
```

Restart Claude Desktop to activate the server.

## Project Structure

```
mcp/
├── src/
│   ├── index.ts              # Main server entry point
│   ├── types/                # Shared type definitions
│   │   └── index.ts          # Prompt types
│   ├── handlers/             # Request handlers
│   │   ├── index.ts          # Handler exports
│   │   └── prompts-handler.ts    # Prompt request handling
│   └── prompts/              # Prompt implementations (one per file)
│       ├── index.ts              # Prompts registry
│       └── add-react-analytics.ts # React analytics setup guide
├── dist/                     # Compiled JavaScript output
├── package.json
├── tsconfig.json
├── TESTING.md               # Detailed testing guide
└── README.md                # This file
```

## Development

### Watch Mode

Auto-rebuild on file changes:

```bash
bun run dev
```

### Type Checking

```bash
bun run type-check
```

### Building

```bash
bun run build
```

## Adding a New Prompt

1. Create `src/prompts/my-prompt.ts`:

```typescript
import { PromptDefinition } from '../types/index.js';

export const myPrompt: PromptDefinition = {
  name: 'my_prompt',
  description: 'Description of this prompt',
  arguments: [
    {
      name: 'param1',
      description: 'First parameter',
      required: true,
    },
  ],
  handler: async (args) => {
    const param1 = args.param1 as string;

    return {
      messages: [
        {
          role: 'user',
          content: {
            type: 'text',
            text: `User message with ${param1}`,
          },
        },
        {
          role: 'assistant',
          content: {
            type: 'text',
            text: 'Assistant response',
          },
        },
      ],
    };
  },
};
```

2. Export from `src/prompts/index.ts`:

```typescript
import { myPrompt } from './my-prompt.js';

export const prompts = [
  // ... existing prompts
  myPrompt,
];
```

3. Rebuild and test:

```bash
bun run build
mcp-inspector node dist/index.js
```

## Architecture

The server follows a modular architecture:

- **Types**: Shared TypeScript interfaces for prompts
- **Handlers**: Handle MCP protocol requests and delegate to prompt implementations
- **Prompts**: Individual prompt implementations in separate files for maintainability

This structure makes it easy to:
- Add new prompts without modifying existing code
- Test individual prompts in isolation
- Understand the codebase at a glance
- Scale to dozens of prompts

## Testing

See [TESTING.md](./TESTING.md) for comprehensive testing instructions.

Quick test:

```bash
# Using MCP Inspector
mcp-inspector node dist/index.js

# Or run the test script
./test-local.sh
```

## Available Prompts

### add_react_analytics

Comprehensive guide to add Temps analytics to a React application with step-by-step instructions for different frameworks.

**Arguments:**
- `framework` (string, required): The React framework being used
  - `nextjs-app` - Next.js App Router (13+)
  - `nextjs-pages` - Next.js Pages Router
  - `vite` - Vite + React
  - `cra` - Create React App
  - `remix` - Remix Framework
- `project_id` (number, optional): The Temps project ID for analytics

**Features Included:**
- Installation and basic setup
- Provider configuration
- Custom event tracking (`useTrackEvent`)
- Scroll visibility tracking (`useScrollVisibility`)
- Page leave tracking with time on page (`usePageLeave`)
- User engagement tracking with heartbeat system (`useEngagementTracking`)
- Session recording with privacy controls (`useSessionRecording`)
- Performance tracking with Web Vitals (`useSpeedAnalytics`)
- Manual pageview tracking (`useTrackPageview`)
- User identification
- Advanced provider configuration
- Comprehensive troubleshooting guide

**Usage:**
```json
{
  "name": "add_react_analytics",
  "arguments": {
    "framework": "nextjs-app",
    "project_id": 1
  }
}
```

**Example Response Includes:**
- Framework-specific installation steps
- Provider setup with configuration options
- Code examples for all tracking hooks
- Privacy and performance considerations
- Debug mode and troubleshooting tips

## Next Steps

- [ ] Connect to actual Temps API for real data
- [ ] Add authentication/authorization
- [ ] Create more useful prompts (debugging, optimization, troubleshooting)
- [ ] Add error handling and retries
- [ ] Write unit tests
- [ ] Publish to npm

## Resources

- [Model Context Protocol Documentation](https://modelcontextprotocol.io/)
- [MCP TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk)
- [MCP Servers Examples](https://github.com/modelcontextprotocol/servers)

## License

MIT
