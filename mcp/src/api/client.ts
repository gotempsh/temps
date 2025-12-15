/**
 * Temps API Client
 * Uses environment variables for configuration
 */

export interface TempsConfig {
  apiUrl: string;
  apiToken: string;
}

export function getConfig(): TempsConfig {
  const apiUrl = process.env.TEMPS_API_URL;
  const apiToken = process.env.TEMPS_API_TOKEN;

  if (!apiUrl) {
    throw new Error('TEMPS_API_URL environment variable is required');
  }

  if (!apiToken) {
    throw new Error('TEMPS_API_TOKEN environment variable is required');
  }

  return { apiUrl, apiToken };
}

export interface Project {
  id: number;
  name: string;
  slug: string;
  repo_owner: string | null;
  repo_name: string | null;
  main_branch: string | null;
  created_at: string;
  updated_at: string;
}

export interface ProjectsResponse {
  projects: Project[];
}

export interface Deployment {
  id: number;
  project_id: number;
  status: string;
  branch: string | null;
  commit_hash: string | null;
  commit_message: string | null;
  url: string | null;
  created_at: number;
}

export interface DeploymentsResponse {
  deployments: Deployment[];
}

export class TempsClient {
  private config: TempsConfig;

  constructor(config?: TempsConfig) {
    this.config = config || getConfig();
  }

  private async request<T>(path: string, options?: RequestInit): Promise<T> {
    const url = `${this.config.apiUrl}${path}`;

    const response = await fetch(url, {
      ...options,
      headers: {
        'Authorization': `Bearer ${this.config.apiToken}`,
        'Content-Type': 'application/json',
        ...options?.headers,
      },
    });

    if (!response.ok) {
      const error = await response.text();
      throw new Error(`API Error (${response.status}): ${error}`);
    }

    return response.json() as Promise<T>;
  }

  async listProjects(page = 1, pageSize = 20): Promise<ProjectsResponse> {
    return this.request<ProjectsResponse>(
      `/projects?page=${page}&page_size=${pageSize}`
    );
  }

  async getProject(projectId: number): Promise<Project> {
    return this.request<Project>(`/projects/${projectId}`);
  }

  async listDeployments(
    projectId: number,
    page = 1,
    pageSize = 20
  ): Promise<DeploymentsResponse> {
    return this.request<DeploymentsResponse>(
      `/projects/${projectId}/deployments?page=${page}&page_size=${pageSize}`
    );
  }
}

// Singleton instance
let client: TempsClient | null = null;

export function getClient(): TempsClient {
  if (!client) {
    client = new TempsClient();
  }
  return client;
}
