/**
 * Prompts Handler
 * Handles prompt-related requests (list and get)
 */

import { GetPromptResult } from '@modelcontextprotocol/sdk/types.js';
import { prompts } from '../prompts/index.js';

export function listPrompts() {
  return {
    prompts: prompts.map((p) => ({
      name: p.name,
      description: p.description,
      arguments: p.arguments,
    })),
  };
}

export async function getPrompt(
  name: string,
  args: Record<string, unknown>
): Promise<GetPromptResult> {
  const prompt = prompts.find((p) => p.name === name);

  if (!prompt) {
    throw new Error(`Unknown prompt: ${name}`);
  }

  return await prompt.handler(args);
}
