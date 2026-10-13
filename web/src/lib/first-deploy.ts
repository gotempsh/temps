// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { startFirstImageDeploy } from './first-image-deploy'

/**
 * The one-click sample application offered to installs that have not deployed
 * anything yet. A small, public, unauthenticated image that serves a page on
 * a known port, so the only things that can stop it are the host itself
 * (Docker, registry access) — exactly what a first deploy should prove.
 */
export const SAMPLE_APP = {
  // Compatible with the default cap-drop=ALL runtime: no root/chown needed.
  image: 'nginxinc/nginx-unprivileged:alpine',
  port: 8080,
  baseName: 'hello-temps',
} as const

/** Where the guided first deploy lives; reachable from the setup checklist. */
export const FIRST_DEPLOY_PATH = '/get-started'

/** The guided page tracking a specific project's first deployment. */
export function firstDeployTrackingPath(projectSlug: string): string {
  const params = new URLSearchParams({ project: projectSlug })
  return `${FIRST_DEPLOY_PATH}?${params.toString()}`
}

/**
 * A project name for the sample that does not collide with an existing one:
 * `hello-temps`, then `hello-temps-2`, `hello-temps-3`, …
 */
export function sampleProjectName(existingNames: readonly string[]): string {
  const taken = new Set(existingNames.map((name) => name.trim().toLowerCase()))
  if (!taken.has(SAMPLE_APP.baseName)) return SAMPLE_APP.baseName
  for (let suffix = 2; ; suffix++) {
    const candidate = `${SAMPLE_APP.baseName}-${suffix}`
    if (!taken.has(candidate)) return candidate
  }
}

/**
 * Whether this installation has ever completed a deployment.
 *
 * Project statistics checks successful completion in deployment history.
 * A project's last_deployment timestamp alone can mean a Git build started.
 */
export function hasCompletedDeployment(
  statistics: { has_completed_deployment?: boolean } | undefined
): boolean {
  return statistics?.has_completed_deployment === true
}

export type FirstDeployPhase = 'in_progress' | 'succeeded' | 'failed'

/** Collapse a deployment status into the three states the guide shows. */
export function firstDeployPhase(status: string | undefined): FirstDeployPhase {
  switch (status) {
    // `superseded`: a later deployment replaced it, which only happens after
    // it went live.
    case 'completed':
    case 'superseded':
      return 'succeeded'
    case 'failed':
    case 'cancelled':
    case 'stopped':
      return 'failed'
    default:
      return 'in_progress'
  }
}

export type FirstDeployFailureKind =
  'docker_unavailable' | 'image_pull' | 'other'

export interface FirstDeployFailureHint {
  kind: FirstDeployFailureKind
  title: string
  remedy: string
}

const DOCKER_UNAVAILABLE_PATTERNS = [
  /cannot connect to the docker daemon/i,
  /docker\.sock/i,
  /is the docker daemon running/i,
  /docker (?:daemon |engine )?(?:is )?(?:not available|unavailable|not running)/i,
  /error trying to connect: .*docker/i,
]

const IMAGE_PULL_PATTERNS = [
  /pull access denied/i,
  /manifest (?:for .* )?(?:not found|unknown)/i,
  /failed to pull/i,
  /error pulling image/i,
  /(?:pull|pulling).*(?:timeout|timed out)/i,
  /toomanyrequests/i,
  /rate limit/i,
  /no such host/i,
  /registry-1\.docker\.io/i,
  /tls handshake timeout/i,
]

/**
 * Turn the raw failure reason of a first deployment into the one next step
 * an operator needs. The reason itself is still shown verbatim; this only
 * adds what to do about the two causes a brand-new host most often hits.
 */
export function firstDeployFailureHint(
  reason: string | null | undefined
): FirstDeployFailureHint {
  const text = reason ?? ''
  if (DOCKER_UNAVAILABLE_PATTERNS.some((pattern) => pattern.test(text))) {
    return {
      kind: 'docker_unavailable',
      title: 'Docker is not reachable from Temps',
      remedy:
        'Temps runs every app in a Docker container. Start Docker on this server (or give the Temps service user access to the Docker socket), then retry.',
    }
  }
  if (IMAGE_PULL_PATTERNS.some((pattern) => pattern.test(text))) {
    return {
      kind: 'image_pull',
      title: 'The image could not be downloaded',
      remedy:
        'This server could not pull the image from its registry. Check that it can reach the internet (or your registry mirror) and is not rate limited, then retry.',
    }
  }
  return {
    kind: 'other',
    title: 'The deployment did not finish',
    remedy: 'Open the deployment to read its logs, fix the cause, then retry.',
  }
}

interface SampleDeployDeps<
  TEnv extends { id: number; name: string; slug: string; is_preview: boolean },
> {
  createProject: (body: {
    name: string
    imageRef: string
    port: number
  }) => Promise<{ id: number; slug: string }>
  listEnvironments: (projectId: number) => Promise<readonly TEnv[]>
  deployImage: (args: {
    projectId: number
    environmentId: number
    imageRef: string
  }) => Promise<unknown>
}

export type SampleDeployResult =
  | { status: 'started'; projectSlug: string; environmentName: string }
  | { status: 'no_environment'; projectSlug: string }
  | { status: 'deploy_failed'; projectSlug: string; error: unknown }

/**
 * Create the sample project and start its first deployment.
 *
 * Project creation errors propagate (nothing was created). Once the project
 * exists, a failure to start the deployment is returned rather than thrown, so
 * the caller can still send the user to the project with the reason.
 */
export async function startSampleDeploy<
  TEnv extends { id: number; name: string; slug: string; is_preview: boolean },
>(name: string, deps: SampleDeployDeps<TEnv>): Promise<SampleDeployResult> {
  const project = await deps.createProject({
    name,
    imageRef: SAMPLE_APP.image,
    port: SAMPLE_APP.port,
  })
  try {
    const result = await startFirstImageDeploy(project.id, SAMPLE_APP.image, {
      listEnvironments: deps.listEnvironments,
      deployImage: deps.deployImage,
    })
    if (result.status === 'started') {
      return {
        status: 'started',
        projectSlug: project.slug,
        environmentName: result.environmentName,
      }
    }
    return { status: 'no_environment', projectSlug: project.slug }
  } catch (error) {
    return { status: 'deploy_failed', projectSlug: project.slug, error }
  }
}
