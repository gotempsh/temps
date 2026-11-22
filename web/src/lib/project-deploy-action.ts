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

/**
 * The upload page's address: the environment it starts on, and every
 * environment still waiting for this source. An upload deploys one environment
 * at a time, so the address carries the rest, and a reload or the back button
 * does not lose them.
 */
export function projectUploadSearch(
  environmentId?: number,
  pendingEnvironmentIds: readonly number[] = []
): URLSearchParams {
  const params = new URLSearchParams()
  if (environmentId != null) params.set('environment', String(environmentId))
  for (const id of pendingEnvironmentIds) params.append('pending', String(id))
  return params
}

/** The upload page, on the environment the upload is for when one was asked for. */
export function projectUploadPath(
  slug: string,
  environmentId?: number,
  pendingEnvironmentIds?: readonly number[]
) {
  const path = `/projects/${slug}/drop`
  const search = projectUploadSearch(environmentId, pendingEnvironmentIds)
  return search.size === 0 ? path : `${path}?${search.toString()}`
}

/**
 * Where a deploy request goes for a project deployed by uploading, carrying
 * along what it asked for; `null` when the project deploys from a dialog.
 */
export function uploadDeployPath(
  project: { slug: string; source_type: SourceType },
  options?: { environmentId?: number; pendingEnvironmentIds?: number[] }
): string | null {
  if (projectDeployLaunchMode(project.source_type) !== 'upload') return null
  return projectUploadPath(
    project.slug,
    options?.environmentId,
    options?.pendingEnvironmentIds
  )
}

/**
 * The environments an upload still has to reach, in the order they were asked
 * for: an id that no longer exists is dropped, and so is the one just handled.
 */
export function pendingUploadEnvironments<T extends { id: number }>(
  environments: readonly T[],
  pending: readonly string[],
  except?: number
): T[] {
  return pending.flatMap((id) =>
    environments.filter((env) => String(env.id) === id && env.id !== except)
  )
}

/**
 * The environment an upload starts on: the one the link asked for, while it
 * still exists, otherwise the usual default.
 */
export function initialUploadEnvironment<
  T extends { id: number; slug: string; is_preview: boolean },
>(environments: readonly T[], requested: string | null): T | undefined {
  return (
    environments.find((env) => String(env.id) === requested) ??
    defaultDeployEnvironment(environments)
  )
}

/**
 * The branch a deploy dialog starts on: the one the chosen environment
 * deploys from, so what it offers is what that environment runs; otherwise
 * the branch the caller passed, otherwise the project's main branch.
 */
export function initialDeployBranch(
  environment: { branch?: string | null } | undefined,
  defaultBranch: string | undefined,
  mainBranch: string | null | undefined
): string {
  return (
    environment?.branch?.trim() || defaultBranch?.trim() || mainBranch || ''
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
