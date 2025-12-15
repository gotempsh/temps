#!/usr/bin/env node

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  ListPromptsRequestSchema,
  GetPromptRequestSchema,
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';

import {
  listPrompts,
  getPrompt,
  listTools,
  callTool,
} from './handlers/index.js';

/**
 * Temps MCP Server
 *
 * Provides MCP prompts and tools for Temps platform operations
 */

const server = new Server(
  {
    name: '@temps-sdk/mcp',
    version: '0.0.1',
  },
  {
    capabilities: {
      prompts: {},
      tools: {},
    },
  }
);

/**
 * List available prompts
 */
server.setRequestHandler(ListPromptsRequestSchema, async () => {
  return listPrompts();
});

/**
 * Get prompt content
 */
server.setRequestHandler(GetPromptRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;
  return await getPrompt(name, args || {});
});

/**
 * List available tools
 */
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return listTools();
});

/**
 * Call a tool
 */
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;
  return await callTool(name, args || {});
});

/**
 * Start the server
 */
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);

  console.error('Temps MCP Server running on stdio');
  console.error(
    `API URL: ${process.env.TEMPS_API_URL || '(not configured)'}`
  );
}

main().catch((error) => {
  console.error('Fatal error:', error);
  process.exit(1);
});
