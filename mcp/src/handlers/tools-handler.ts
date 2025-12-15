/**
 * Tools handler for Temps MCP Server
 */

import { getClient } from '../api/index.js';

export interface Tool {
  name: string;
  description: string;
  inputSchema: {
    type: 'object';
    properties: Record<string, unknown>;
    required?: string[];
  };
}

export const tools: Tool[] = [
  {
    name: 'list_projects',
    description: 'List all projects in the Temps platform',
    inputSchema: {
      type: 'object',
      properties: {
        page: {
          type: 'number',
          description: 'Page number (default: 1)',
        },
        page_size: {
          type: 'number',
          description: 'Number of items per page (default: 20, max: 100)',
        },
      },
    },
  },
  {
    name: 'get_project',
    description: 'Get details of a specific project by ID',
    inputSchema: {
      type: 'object',
      properties: {
        project_id: {
          type: 'number',
          description: 'The project ID',
        },
      },
      required: ['project_id'],
    },
  },
  {
    name: 'list_deployments',
    description: 'List deployments for a specific project',
    inputSchema: {
      type: 'object',
      properties: {
        project_id: {
          type: 'number',
          description: 'The project ID',
        },
        page: {
          type: 'number',
          description: 'Page number (default: 1)',
        },
        page_size: {
          type: 'number',
          description: 'Number of items per page (default: 20, max: 100)',
        },
      },
      required: ['project_id'],
    },
  },
];

export function listTools() {
  return { tools };
}

export async function callTool(
  name: string,
  args: Record<string, unknown>
): Promise<{ content: Array<{ type: 'text'; text: string }> }> {
  const client = getClient();

  try {
    switch (name) {
      case 'list_projects': {
        const page = (args.page as number) || 1;
        const pageSize = (args.page_size as number) || 20;
        const result = await client.listProjects(page, pageSize);

        const projectList = result.projects
          .map(
            (p) =>
              `- **${p.name}** (ID: ${p.id}, Slug: ${p.slug})${p.repo_owner && p.repo_name ? ` - ${p.repo_owner}/${p.repo_name}` : ''}`
          )
          .join('\n');

        return {
          content: [
            {
              type: 'text',
              text: `## Projects (Page ${page})\n\nFound: ${result.projects.length} projects\n\n${projectList || 'No projects found.'}`,
            },
          ],
        };
      }

      case 'get_project': {
        const projectId = args.project_id as number;
        if (!projectId) {
          throw new Error('project_id is required');
        }
        const project = await client.getProject(projectId);

        return {
          content: [
            {
              type: 'text',
              text: `## Project: ${project.name}\n\n- **ID**: ${project.id}\n- **Slug**: ${project.slug}\n- **Repository**: ${project.repo_owner && project.repo_name ? `${project.repo_owner}/${project.repo_name}` : 'Not connected'}\n- **Main Branch**: ${project.main_branch || 'Not set'}\n- **Created**: ${project.created_at}\n- **Updated**: ${project.updated_at}`,
            },
          ],
        };
      }

      case 'list_deployments': {
        const projectId = args.project_id as number;
        if (!projectId) {
          throw new Error('project_id is required');
        }
        const page = (args.page as number) || 1;
        const pageSize = (args.page_size as number) || 20;
        const result = await client.listDeployments(projectId, page, pageSize);

        const deploymentList = result.deployments
          .map(
            (d) =>
              `- **Deployment #${d.id}** - Status: ${d.status}${d.branch ? `, Branch: ${d.branch}` : ''}${d.commit_hash ? `, Commit: ${d.commit_hash.substring(0, 7)}` : ''}${d.url ? ` - [${d.url}](${d.url})` : ''}`
          )
          .join('\n');

        return {
          content: [
            {
              type: 'text',
              text: `## Deployments for Project ${projectId} (Page ${page})\n\nFound: ${result.deployments.length} deployments\n\n${deploymentList || 'No deployments found.'}`,
            },
          ],
        };
      }

      default:
        throw new Error(`Unknown tool: ${name}`);
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      content: [
        {
          type: 'text',
          text: `Error: ${message}`,
        },
      ],
    };
  }
}
