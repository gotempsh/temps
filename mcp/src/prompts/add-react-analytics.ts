/**
 * Prompt: Add React Analytics
 * Guides users through adding Temps analytics to their React application
 */

import { PromptDefinition } from '../types/index.js';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// Get the directory of the current module
const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Load markdown templates
const templatesDir = join(__dirname, 'templates');

const loadTemplate = (filename: string): string => {
  try {
    return readFileSync(join(templatesDir, filename), 'utf-8');
  } catch (error) {
    console.error(`Failed to load template ${filename}:`, error);
    return `Error: Could not load template for ${filename}`;
  }
};

// Load all templates once at module initialization
const templates = {
  'nextjs-app': loadTemplate('nextjs-app.md'),
  'nextjs-pages': loadTemplate('nextjs-pages.md'),
  vite: loadTemplate('vite.md'),
  cra: loadTemplate('cra.md'),
  remix: loadTemplate('remix.md'),
  'advanced-features': loadTemplate('advanced-features.md'),
};

export const addReactAnalyticsPrompt: PromptDefinition = {
  name: 'add_react_analytics',
  description: 'Guide to add Temps analytics to a React application',
  arguments: [
    {
      name: 'framework',
      description:
        'The React framework being used (nextjs-app, nextjs-pages, vite, cra, remix)',
      required: true,
    },
    {
      name: 'project_id',
      description: 'The Temps project ID for analytics',
      required: false,
    },
  ],
  handler: async (args) => {
    const framework = (args.framework as string).toLowerCase();
    const projectId = args.project_id as number | undefined;

    // Get framework-specific instructions (fallback to nextjs-app)
    const frameworkInstructions =
      templates[framework as keyof typeof templates] || templates['nextjs-app'];

    const projectInfo = projectId
      ? `\n\n**Project ID**: ${projectId}\n**Analytics Dashboard**: View your analytics at Temps dashboard for project ${projectId}`
      : '';

    return {
      messages: [
        {
          role: 'user',
          content: {
            type: 'text',
            text: `I want to add Temps analytics to my React application. I'm using ${framework} framework.${projectInfo ? ` My project ID is ${projectId}.` : ''}`,
          },
        },
        {
          role: 'assistant',
          content: {
            type: 'text',
            text: `I'll help you add Temps analytics to your ${framework} application!

${frameworkInstructions}${projectInfo}

${templates['advanced-features']}`,
          },
        },
      ],
    };
  },
};
