// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps providers` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  checkCommitExists,
  commitRepositoryFiles,
  completeConnectionReauthorization,
  createGitProvider,
  createGitlabOauthProvider,
  createRepository,
  detectPublicEnvExample,
  getAllRepositoriesByName,
  getBranchesByRepositoryId,
  getPublicBranches,
  getPublicComposePreview,
  getPublicComposeServices,
  getPublicRepository,
  getRepositoryBranches,
  getRepositoryById,
  getRepositoryComposePreview,
  getRepositoryComposeServicesLive,
  getRepositoryEnvExampleLive,
  getRepositoryPresetByName,
  getRepositoryTags,
  getTagsByRepositoryId,
  listRepositoriesByProvider,
  openRepositoryPullRequest,
  reauthorizeConnection,
  runConnectionHealthCheck,
  startGithubManifestFlow,
  updateGitProviderCredentials,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerProvidersEndpoints(program: Command): void {
  const group = topGroup(program, 'providers', "providers")

  endpoint(group, completeConnectionReauthorization, {
    name: "git complete-connection-reauthorization",
    description: "Complete connection reauthorization",
    body: {
      code: { kind: 'string!', help: "Empty when the user cancelled at the provider; the state is still consumed" },
      state: 'string!',
    },
  })

  endpoint(group, runConnectionHealthCheck, {
    name: "connections run-health-check",
    description: "Run an on-demand health check for a git connection",
    path: { connection_id: 'int' },
  })

  endpoint(group, reauthorizeConnection, {
    name: "git reauthorize-connection",
    description: "Reauthorize connection",
    path: { connection_id: 'int' },
  })

  endpoint(group, createRepository, {
    name: "connections create-repository",
    description: "Create an empty repository through one of your git connections",
    path: { connection_id: 'int' },
    body: {
      description: { kind: 'string', help: "Short description, up to 350 characters" },
      name: { kind: 'string!', help: "Repository name: letters, digits, '-', '_' and '.', up to 100 characters" },
      owner: { kind: 'string', help: "Account or organization to create it under" },
      private: { kind: 'bool', help: "Whether the repository is private" },
    },
  })

  endpoint(group, commitRepositoryFiles, {
    name: "connections commit-repository-files",
    description: "Commit files to a repository through one of your git connections",
    path: { connection_id: 'int', owner: 'string', repo: 'string' },
    body: {
      branch: { kind: 'string', help: "Branch to commit to" },
      files: { kind: 'json!', help: "Files to add or overwrite (1-100, at most 5 MiB in total)" },
      message: { kind: 'string!', help: "Commit message (at most 500 bytes)" },
    },
  })

  endpoint(group, openRepositoryPullRequest, {
    name: "connections open-repository-pull-request",
    description: "Open a pull request through one of your git connections",
    path: { connection_id: 'int', owner: 'string', repo: 'string' },
    body: {
      base_branch: { kind: 'string', help: "Branch to merge into" },
      body: 'string',
      head_branch: { kind: 'string!', help: "Branch holding the changes" },
      title: 'string!',
    },
  })

  endpoint(group, createGitProvider, {
    name: "git create",
    description: "Create a new git provider configuration",
    body: {
      config: 'json!',
      enabled: 'bool',
      name: 'string!',
      provider_type: 'string!',
    },
  })

  endpoint(group, startGithubManifestFlow, {
    name: "git start-github-manifest-flow",
    description: "Start creating a GitHub App from a manifest",
  })

  endpoint(group, createGitlabOauthProvider, {
    name: "git create-gitlab-oauth",
    description: "Create a GitLab OAuth provider",
    body: {
      base_url: 'string',
      client_id: 'string!',
      client_secret: { kind: 'secret!', help: "A credential" },
      name: 'string!',
      redirect_uri: 'string!',
    },
  })

  endpoint(group, updateGitProviderCredentials, {
    name: "git update-credentials",
    description: "Partially update credentials for an existing git provider",
    path: { provider_id: 'int' },
    body: {
      app_id: { kind: 'string', help: "Application ID (GitHub App integer as string; GitLab App string)" },
      app_secret: { kind: 'secret', help: "A credential" },
      client_id: { kind: 'string', help: "OAuth client ID (GitLab OAuth, GitHub App)" },
      client_secret: { kind: 'secret', help: "A credential" },
      password: { kind: 'secret', help: "A credential" },
      private_key: { kind: 'secret', help: "A credential" },
      public_key: { kind: 'string', help: "SSH public key (one OpenSSH line) of an SSH-key provider" },
      redirect_uri: { kind: 'string', help: "OAuth redirect URI (GitLab OAuth / GitLab App)" },
      token: { kind: 'secret', help: "A credential" },
      username: { kind: 'string', help: "Username of a basic-auth provider (generic Git server, Bitbucket app password)" },
      webhook_secret: { kind: 'secret', help: "A credential" },
    },
  })

  endpoint(group, listRepositoriesByProvider, {
    name: "git list-repositories-by",
    description: "List all repositories for a specific provider",
    path: { provider_id: 'int' },
    query: {
      page: { kind: 'int', help: "Page number for pagination" },
      per_page: { kind: 'int', help: "Number of items per page (max 100)" },
      sort: { kind: 'string', help: "Sort field (name, created_at, updated_at, stars, watchers, size, issues)" },
      direction: { kind: 'string', help: "Sort direction (asc, desc)" },
      search: { kind: 'string', help: "Search term to filter repositories" },
      owner: { kind: 'string', help: "Filter by repository owner" },
      language: { kind: 'string', help: "Filter by programming language" },
      private: { kind: 'bool', help: "Filter by private status (true/false)" },
      preset: { kind: 'string', help: "Cached default-branch preset slug; __undetected__ means not inspected" },
      updated_after: { kind: 'string', help: "Updated on or after this RFC3339 timestamp" },
    },
  })

  endpoint(group, getPublicRepository, {
    name: "public-repos get-repository",
    description: "Get information about a public repository (supports GitHub and GitLab)",
    path: { provider: 'string', owner: 'string', repo: 'string' },
    query: {
      base_url: { kind: 'string', help: "HTTPS/443 origin of a self-hosted GitLab instance" },
    },
  })

  endpoint(group, getPublicBranches, {
    name: "public-repos get-branches",
    description: "Get branches for a public repository (supports GitHub and GitLab)",
    path: { provider: 'string', owner: 'string', repo: 'string' },
    query: {
      base_url: { kind: 'string', help: "HTTPS/443 origin of a self-hosted GitLab instance" },
      fresh: { kind: 'bool', help: "Force fetch fresh data, bypassing cache (default: false)" },
    },
  })

  endpoint(group, getPublicComposeServices, {
    name: "public-repos get-compose-services",
    description: "Parse a compose file's services for a public repository (supports GitHub and GitLab)",
    path: { provider: 'string', owner: 'string', repo: 'string' },
    query: {
      base_url: { kind: 'string', help: "HTTPS/443 origin of a self-hosted GitLab instance" },
      branch: { kind: 'string', help: "Branch to read the compose file from (default: repository's default branch)" },
      path: { kind: 'string!', help: "Compose file path to fetch and parse (from the 'compose_files' list '/preset' already returned, or a..." },
    },
  })

  endpoint(group, getPublicComposePreview, {
    name: "public-repos get-compose-preview",
    description: "Render a redacted effective Compose preview for a public repository",
    path: { provider: 'string', owner: 'string', repo: 'string' },
    query: {
      base_url: { kind: 'string', help: "HTTPS/443 origin of a self-hosted GitLab instance" },
    },
    body: {
      branch: 'string',
      composeOverride: 'string',
      excludedServices: 'strings',
      path: 'string!',
      previewPolicy: { kind: 'json', help: "Advisory preview only" },
    },
  })

  endpoint(group, detectPublicEnvExample, {
    name: "public-repos detect-env-example",
    description: "Detect and parse a '.env.example'-style file for a public repository (supports GitHub and GitLab)",
    path: { provider: 'string', owner: 'string', repo: 'string' },
    query: {
      base_url: { kind: 'string', help: "HTTPS/443 origin of a self-hosted GitLab instance" },
      branch: { kind: 'string', help: "Branch name to inspect (default: repository's default branch)" },
      root_directory: { kind: 'string', help: "Project root directory to search (default: repository root)" },
    },
  })

  endpoint(group, getAllRepositoriesByName, {
    name: "git get-all-repositories-by-name",
    description: "Get all repositories with same owner/name from all git providers",
    path: { owner: 'string', name: 'string' },
  })

  endpoint(group, getRepositoryPresetByName, {
    name: "git get-repository-preset-by-name",
    description: "Get repository preset by owner and name",
    path: { owner: 'string', name: 'string' },
    query: {
      branch: { kind: 'string', help: "Git branch to check (defaults to repository's default branch)" },
    },
  })

  endpoint(group, getRepositoryBranches, {
    name: "repos get-repository-branches",
    description: "Get repository branches",
    path: { owner: 'string', repo: 'string' },
    query: {
      connection_id: { kind: 'int!', help: "Git provider connection ID (required when multiple connections have the same repo)" },
      fresh: { kind: 'bool', help: "Force fetch fresh data, bypassing cache (default: false)" },
    },
  })

  endpoint(group, getRepositoryTags, {
    name: "repos get-repository-tags",
    description: "Get repository tags",
    path: { owner: 'string', repo: 'string' },
    query: {
      connection_id: { kind: 'int!', help: "Git provider connection ID (required when multiple connections have the same repo)" },
      fresh: { kind: 'bool', help: "Force fetch fresh data, bypassing cache (default: false)" },
    },
  })

  endpoint(group, getRepositoryComposeServicesLive, {
    name: "git get-repository-compose-services-live",
    description: "Parse a compose file's services for a connected repository, live",
    path: { repository_id: 'int' },
    query: {
      branch: { kind: 'string', help: "Git branch to check (defaults to repository's default branch)" },
      path: { kind: 'string!', help: "Compose file path to fetch and parse" },
    },
  })

  endpoint(group, getRepositoryComposePreview, {
    name: "git get-repository-compose-preview",
    description: "Render a redacted effective Compose preview for a connected repository",
    path: { repository_id: 'int' },
    body: {
      branch: 'string',
      composeOverride: 'string',
      environmentId: { kind: 'int', help: "Environment whose variables resolve '${VAR}'; defaults to the project's production environment" },
      excludedServices: 'strings',
      path: 'string!',
      previewPolicy: { kind: 'json', help: "Advisory preview only" },
      projectId: { kind: 'int', help: "The project this Compose file deploys" },
    },
  })

  endpoint(group, getRepositoryEnvExampleLive, {
    name: "git get-repository-env-example-live",
    description: "Detect and parse a '.env.example'-style file for a connected repository",
    path: { repository_id: 'int' },
    query: {
      branch: { kind: 'string', help: "Git branch to check (defaults to repository's default branch)" },
      root_directory: { kind: 'string', help: "Project root directory to search (defaults to repository root)" },
    },
  })

  endpoint(group, getRepositoryById, {
    name: "git get-repository-by-id",
    description: "Get repository by ID",
    path: { repository_id: 'int' },
  })

  endpoint(group, getBranchesByRepositoryId, {
    name: "repos get-branches-by-repository-id",
    description: "Get repository branches by repository ID",
    path: { repository_id: 'int' },
    query: {
      fresh: { kind: 'bool', help: "Force fetch fresh data, bypassing cache (default: false)" },
    },
  })

  endpoint(group, checkCommitExists, {
    name: "repos check-commit-exists",
    description: "Check if a commit exists in a repository",
    path: { repository_id: 'int', commit_sha: 'string' },
  })

  endpoint(group, getTagsByRepositoryId, {
    name: "repos get-tags-by-repository-id",
    description: "Get repository tags by repository ID",
    path: { repository_id: 'int' },
    query: {
      fresh: { kind: 'bool', help: "Force fetch fresh data, bypassing cache (default: false)" },
    },
  })
}
