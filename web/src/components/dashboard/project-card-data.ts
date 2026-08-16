import type { ProjectResponse } from '@/api/client'

type RepositoryProject = Pick<
  ProjectResponse,
  'git_url' | 'repo_name' | 'repo_owner'
>

export type RepositoryInfo = {
  label: string
  provider: 'github' | 'gitlab' | 'git'
}

export type ProjectBuildSource = {
  label: string
  kind: 'github' | 'gitlab' | 'git' | 'docker' | 'source'
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

  const lowerUrl = gitUrl?.toLowerCase() ?? ''
  const provider = lowerUrl.includes('gitlab')
    ? 'gitlab'
    : lowerUrl.includes('github') || !gitUrl
      ? 'github'
      : 'git'

  return { label: `${owner}/${name}`, provider }
}

export function projectBuildSource(
  project: Pick<
    ProjectResponse,
    'source_type' | 'git_url' | 'repo_name' | 'repo_owner'
  >
): ProjectBuildSource {
  if (project.source_type === 'git') {
    const provider = projectRepository(project)?.provider ?? 'git'
    return {
      kind: provider,
      label:
        provider === 'github'
          ? 'GitHub'
          : provider === 'gitlab'
            ? 'GitLab'
            : 'Git repository',
    }
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
      return 'Deployed'
    case 'running':
    case 'pending':
      return 'Deploying, started'
    default:
      return 'Last attempt'
  }
}
