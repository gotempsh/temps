// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'

type RepositoryProject = Pick<
  ProjectResponse,
  'git_url' | 'repo_name' | 'repo_owner' | 'git_provider_type'
>

export type GitProvider = 'github' | 'gitlab' | 'gitea' | 'bitbucket' | 'git'

export type RepositoryInfo = {
  label: string
  provider: GitProvider
}

export type ProjectBuildSource = {
  label: string
  kind: GitProvider | 'docker' | 'source'
}

const PROVIDER_LABELS: Record<GitProvider, string> = {
  github: 'GitHub',
  gitlab: 'GitLab',
  gitea: 'Gitea',
  bitbucket: 'Bitbucket',
  git: 'Git repository',
}

/**
 * The provider type stored on the project's Git connection, normalized to what
 * the UI draws. `github_app` is the GitHub App flavour of GitHub; `generic`
 * (self-hosted plain Git) has no brand mark of its own.
 */
function providerFromConnection(
  providerType: string | null | undefined
): GitProvider | null {
  switch (providerType?.toLowerCase()) {
    case 'github':
    case 'github_app':
      return 'github'
    case 'gitlab':
      return 'gitlab'
    case 'gitea':
      return 'gitea'
    case 'bitbucket':
      return 'bitbucket'
    case 'generic':
      return 'git'
    default:
      return null
  }
}

/**
 * Last-resort hostname sniffing, for public repositories that have a clone URL
 * but no connection to read the provider from.
 *
 * This can only ever recognize vendors that name themselves in the hostname, so
 * it must never be used when `git_provider_type` is available, and an
 * unrecognized host must stay generic — a self-hosted instance on a custom
 * domain is not GitHub just because we failed to identify it.
 */
function providerFromUrl(gitUrl: string | undefined): GitProvider {
  const url = gitUrl?.toLowerCase() ?? ''
  if (url.includes('github')) return 'github'
  if (url.includes('gitlab')) return 'gitlab'
  if (url.includes('bitbucket')) return 'bitbucket'
  if (url.includes('gitea')) return 'gitea'
  return 'git'
}

export function projectPresetLabel(preset?: string | null): string {
  if (!preset) return 'Not configured'

  const labels: Record<string, string> = {
    nextjs: 'Next.js',
    'docker-compose': 'Docker Compose',
    'nixpacks-node': 'Nixpacks Node',
  }

  return (
    labels[preset.toLowerCase()] ??
    preset
      .replace(/[-_]+/g, ' ')
      .replace(/\b\w/g, (character) => character.toUpperCase())
  )
}

export function projectRepository(
  project: RepositoryProject
): RepositoryInfo | null {
  const gitUrl = project.git_url?.trim()
  const normalizedUrl = gitUrl?.replace(/\.git$/i, '')
  const urlParts = normalizedUrl?.match(/(?:[:/])([^/:]+)\/([^/]+)$/)
  const owner = project.repo_owner?.trim() || urlParts?.[1]
  const name = project.repo_name?.trim() || urlParts?.[2]

  if (!owner || !name) return null

  // The connected provider is authoritative; the clone URL is only a fallback
  // for public repositories, which have no connection to read it from.
  const provider =
    providerFromConnection(project.git_provider_type) ?? providerFromUrl(gitUrl)

  return { label: `${owner}/${name}`, provider }
}

export function projectBuildSource(
  project: Pick<
    ProjectResponse,
    'source_type' | 'git_url' | 'repo_name' | 'repo_owner' | 'git_provider_type'
  >
): ProjectBuildSource {
  if (project.source_type === 'git') {
    const provider =
      providerFromConnection(project.git_provider_type) ??
      projectRepository(project)?.provider ??
      'git'
    return { kind: provider, label: PROVIDER_LABELS[provider] }
  }

  if (project.source_type === 'docker_image') {
    return { kind: 'docker', label: 'Docker image' }
  }

  return { kind: 'source', label: 'Source upload' }
}

/**
 * Only a completed deployment gets to be called "Deployed". A failed attempt
 * still updates `last_deployment`, so calling every timestamp a deployment
 * would contradict its status.
 */
export function deploymentLabel(status?: string | null): string {
  switch (status) {
    case 'completed':
    case 'deployed':
      return 'Deployed'
    case 'running':
    case 'pending':
      return 'Deploying, started'
    default:
      return 'Last attempt'
  }
}
