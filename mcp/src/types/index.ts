/**
 * Shared types for Temps MCP Server
 */

import {
  TextContent,
  ImageContent,
  EmbeddedResource,
} from '@modelcontextprotocol/sdk/types.js';

export interface PromptDefinition {
  name: string;
  description: string;
  arguments?: Array<{
    name: string;
    description: string;
    required?: boolean;
  }>;
  handler: (args: Record<string, unknown>) => Promise<{
    messages: Array<{
      role: 'user' | 'assistant';
      content: TextContent | ImageContent | EmbeddedResource;
    }>;
  }>;
}
