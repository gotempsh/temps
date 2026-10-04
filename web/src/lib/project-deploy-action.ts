// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceType } from '@/api/client'

export function projectDeployLaunchMode(
  sourceType: SourceType
): 'dialog' | 'upload' {
  return sourceType === 'uploaded_source' || sourceType === 'static_files'
    ? 'upload'
    : 'dialog'
}

/**
 * Whether a brand-new deployment of this project pulls a prebuilt image
 * rather than running the git pipeline.
 *
 * A Flexible (`manual`) project created without a repository has nothing for
 * the git pipeline to build — triggering it fails with "Project must have
 * repository information" — so it deploys an image, exactly like a
 * `docker_image` project. Once a repository is attached it uses git.
 */
export function projectDeploysImage(project: {
  source_type: SourceType
  repo_name?: string | null
}): boolean {
  if (project.source_type === 'docker_image') return true
  return project.source_type === 'manual' && !project.repo_name?.trim()
}

/**
 * The environment a one-click deploy should target when the user has not
 * picked one: the production environment if there is one, otherwise the
 * first non-preview environment, otherwise the first environment.
 */
export function defaultDeployEnvironment<
  T extends { slug: string; is_preview: boolean },
>(environments: readonly T[] | undefined): T | undefined {
  if (!environments?.length) return undefined
  const nonPreview = environments.filter((env) => !env.is_preview)
  return (
    nonPreview.find((env) => env.slug === 'production') ??
    nonPreview[0] ??
    environments[0]
  )
}

export function deploymentsAfterStartPath(projectSlug: string): string {
  return `/projects/${projectSlug}/deployments?autoRefresh=true`
}

/**
 * The deployments page with the image dialog open and prefilled, so a first
 * image deployment that failed to start can be retried without retyping it.
 */
export function imageDeployRetryPath(
  projectSlug: string,
  imageRef: string
): string {
  const params = new URLSearchParams({ deploy: 'true', image: imageRef })
  return `/projects/${projectSlug}/deployments?${params.toString()}`
}

/**
 * How many one-second refetches `?autoRefresh=true` makes at most. Callers
 * navigate there right after starting a deployment, which is usually already
 * in the first fetch, so the count may never grow: polling must end anyway.
 */
export const AUTO_REFRESH_MAX_POLLS = 10

export function shouldStopAutoRefresh({
  initialCount,
  currentCount,
  polls,
}: {
  initialCount: number
  currentCount: number
  polls: number
}): boolean {
  return currentCount > initialCount || polls >= AUTO_REFRESH_MAX_POLLS
}
