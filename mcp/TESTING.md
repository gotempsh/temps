# Testing the Temps MCP Server

This guide explains how to test the Temps MCP server (prompts-only) locally.

## Prerequisites

- Node.js 18+ or Bun
- MCP Inspector (for interactive testing)

## Installation

Install dependencies:

```bash
bun install
```

Build the server:

```bash
bun run build
```

## Testing Methods

### Method 1: Using MCP Inspector (Recommended)

The MCP Inspector is the official tool for testing MCP servers interactively.

#### Install MCP Inspector

```bash
npm install -g @modelcontextprotocol/inspector
```

#### Run the Inspector

```bash
mcp-inspector node dist/index.js
```

This will open a web interface where you can:
- View all available prompts
- Test prompts with custom parameters
- See request/response logs

### Method 2: Using Claude Desktop

Add the MCP server to your Claude Desktop configuration:

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

Restart Claude Desktop, and the Temps MCP server will be available.

### Method 3: Manual Testing with stdio

You can test the server by sending JSON-RPC messages via stdin:

1. Start the server:
```bash
node dist/index.js
```

2. Send a request (example for listing prompts):
```json
{"jsonrpc":"2.0","id":1,"method":"prompts/list"}
```

3. The server will respond with the list of available prompts.

### Method 4: Development Mode

Run the server in watch mode during development:

```bash
bun run dev
```

This automatically rebuilds when you change source files.

## Testing Prompts

### List all prompts

**Request:**
```json
{"jsonrpc":"2.0","id":1,"method":"prompts/list"}
```

**Expected Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "prompts": [
      {
        "name": "add_react_analytics",
        "description": "Guide to add Temps analytics to a React application",
        "arguments": [
          {
            "name": "framework",
            "description": "The React framework being used (nextjs-app, nextjs-pages, vite, cra, remix)",
            "required": true
          },
          {
            "name": "project_id",
            "description": "The Temps project ID for analytics",
            "required": false
          }
        ]
      }
    ]
  }
}
```

### Get add_react_analytics prompt

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "prompts/get",
  "params": {
    "name": "add_react_analytics",
    "arguments": {
      "framework": "nextjs-app",
      "project_id": 123
    }
  }
}
```

**Expected Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "messages": [
      {
        "role": "user",
        "content": {
          "type": "text",
          "text": "I want to add Temps analytics to my React application. I'm using nextjs-app framework. My project ID is 123."
        }
      },
      {
        "role": "assistant",
        "content": {
          "type": "text",
          "text": "I'll help you add Temps analytics to your nextjs-app application!\n\n## Adding Analytics to Next.js (App Router 13+)\n\n[Detailed step-by-step instructions including installation, provider setup, event tracking, user identification, and verification...]"
        }
      }
    ]
  }
}
```

**Example without project_id:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "prompts/get",
  "params": {
    "name": "add_react_analytics",
    "arguments": {
      "framework": "vite"
    }
  }
}
```

## Expected Prompt Response Format

All prompts return a response in this format:

```json
{
  "messages": [
    {
      "role": "user",
      "content": {
        "type": "text",
        "text": "User message content"
      }
    },
    {
      "role": "assistant",
      "content": {
        "type": "text",
        "text": "Assistant message content"
      }
    }
  ]
}
```

## Debugging

### Enable Debug Logging

Set the `DEBUG` environment variable:

```bash
DEBUG=* node dist/index.js
```

### Check TypeScript Compilation

```bash
bun run type-check
```

### Common Issues

**Issue: Module not found errors**
- Solution: Make sure you've run `bun run build`
- Check that `dist/` folder exists

**Issue: Server not responding**
- Solution: Verify the server started without errors (check stderr)
- Make sure you're sending properly formatted JSON-RPC

**Issue: Type errors in development**
- Solution: Run `bun run type-check` to see all TypeScript errors
- Update handler types to match MCP SDK expectations

## Adding New Prompts

### Step-by-Step Guide

1. Create a new file in `src/prompts/` (e.g., `my-new-prompt.ts`)

```typescript
import { PromptDefinition } from '../types/index.js';

export const myNewPrompt: PromptDefinition = {
  name: 'my_new_prompt',
  description: 'Description of what this prompt does',
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
            text: `User prompt with ${param1}`,
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
import { myNewPrompt } from './my-new-prompt.js';

export const prompts = [
  // ... existing prompts
  myNewPrompt,
];
```

3. Rebuild and test:

```bash
bun run build
mcp-inspector node dist/index.js
```

## Project Structure

```
mcp/
├── src/
│   ├── index.ts              # Main server entry point
│   ├── types/                # Type definitions
│   │   └── index.ts
│   ├── handlers/             # Request handlers
│   │   ├── index.ts
│   │   └── prompts-handler.ts
│   └── prompts/              # Prompt implementations (one per file)
│       ├── index.ts
│       └── add-react-analytics.ts
└── dist/                     # Compiled JavaScript
```

## Next Steps

Once local testing is complete, you can:

1. Connect to the actual Temps API for real data
2. Add authentication/authorization
3. Implement more prompts for different use cases
4. Add error handling and validation
5. Publish to npm for easier installation
6. Add unit tests
