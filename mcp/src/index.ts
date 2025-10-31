#!/usr/bin/env node

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  ListResourcesRequestSchema,
  ReadResourceRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';

/**
 * Temps MCP Server
 *
 * Provides MCP integration for Temps platform operations
 */

const server = new Server(
  {
    name: '@temps-sdk/mcp',
    version: '0.0.1',
  },
  {
    capabilities: {
      tools: {},
      resources: {},
    },
  }
);

/**
 * List available tools
 */
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return {
    tools: [
      {
        name: 'get_project_info',
        description: 'Get information about a Temps project',
        inputSchema: {
          type: 'object',
          properties: {
            project_id: {
              type: 'number',
              description: 'The ID of the project',
            },
          },
          required: ['project_id'],
        },
      },
      {
        name: 'list_deployments',
        description: 'List deployments for a project',
        inputSchema: {
          type: 'object',
          properties: {
            project_id: {
              type: 'number',
              description: 'The ID of the project',
            },
            limit: {
              type: 'number',
              description: 'Maximum number of deployments to return',
              default: 10,
            },
          },
          required: ['project_id'],
        },
      },
    ],
  };
});

/**
 * Handle tool calls
 */
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  if (!args) {
    throw new Error('Missing arguments');
  }

  switch (name) {
    case 'get_project_info': {
      const projectId = args.project_id as number;

      return {
        content: [
          {
            type: 'text',
            text: `Project information for project ID: ${projectId}\n\nThis is a placeholder implementation. Connect to Temps API to fetch real data.`,
          },
        ],
      };
    }

    case 'list_deployments': {
      const projectId = args.project_id as number;
      const limit = (args.limit as number) || 10;

      return {
        content: [
          {
            type: 'text',
            text: `Listing up to ${limit} deployments for project ${projectId}\n\nThis is a placeholder implementation. Connect to Temps API to fetch real data.`,
          },
        ],
      };
    }

    default:
      throw new Error(`Unknown tool: ${name}`);
  }
});

/**
 * List available resources
 */
server.setRequestHandler(ListResourcesRequestSchema, async () => {
  return {
    resources: [
      {
        uri: 'temps://projects',
        name: 'Temps Projects',
        description: 'List of all Temps projects',
        mimeType: 'application/json',
      },
    ],
  };
});

/**
 * Read resource content
 */
server.setRequestHandler(ReadResourceRequestSchema, async (request) => {
  const { uri } = request.params;

  switch (uri) {
    case 'temps://projects':
      return {
        contents: [
          {
            uri,
            mimeType: 'application/json',
            text: JSON.stringify(
              {
                projects: [
                  {
                    id: 1,
                    name: 'Example Project',
                    description: 'This is a placeholder project',
                  },
                ],
              },
              null,
              2
            ),
          },
        ],
      };

    default:
      throw new Error(`Unknown resource: ${uri}`);
  }
});

/**
 * Start the server
 */
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);

  console.error('Temps MCP Server running on stdio');
}

main().catch((error) => {
  console.error('Fatal error:', error);
  process.exit(1);
});
