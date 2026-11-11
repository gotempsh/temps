// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Linking or unlinking a managed service changes the environment variables a
 * project's app receives, but a running container keeps the variables it was
 * started with: the change only reaches the app on its next deployment. These
 * helpers find what is running and redeploy it unchanged, so the console can
 * offer "Redeploy now" right where the link was changed.
 */

import type {
  DeploymentResponse,
  EnvironmentResponse,
  SourceType,
} from '@/api/client'
import {
  deployFromImage,
  deployFromStatic,
  getDeployment,
  getEnvironments,
  triggerProjectPipeline,
} from '@/api/client/sdk.gen'
import { deploymentRedeployPlan } from '@/lib/deployment-source-summary'
import { historicalImageRuntime } from '@/lib/template-runtime-defaults'

export type ServiceLinkChange = {
  kind: 'linked' | 'unlinked'
  /** Display name of the service whose link changed. */
  serviceName: string
}

/**
 * Environments whose running deployment predates the link change. Preview
 * environments are left out: they are rebuilt from their branch on the next
 * push and are usually idle, so restarting every one of them is not what a
 * single link change asks for.
 */
export function environmentsToRedeploy(
  environments: EnvironmentResponse[] | undefined
): EnvironmentResponse[] {
  return (environments ?? []).filter(
    (environment) =>
      environment.current_deployment_id != null && !environment.is_preview
  )
}

/**
 * The prompt shown after a link change, e.g. "Postgres was linked. …". When
 * the project cannot be redeployed in place, it says to upload the source
 * again instead of offering a redeploy that would be declined.
 */
export function serviceLinkRedeployMessage(
  change: ServiceLinkChange,
  environments: Pick<EnvironmentResponse, 'name'>[],
  redeployable = true
): { title: string; description: string } {
  const verb = change.kind === 'linked' ? 'linked' : 'unlinked'
  const effect =
    change.kind === 'linked'
      ? 'Its connection variables reach your app on its next deployment.'
      : 'Its connection variables stay in the running app until its next deployment.'
  const names = environments.map((environment) => environment.name)
  const where =
    names.length === 0
      ? ''
      : !redeployable
        ? ' Upload the source again to apply the change now.'
        : names.length === 1
          ? ` Redeploy ${names[0]} to apply the change now.`
          : ` Redeploy ${names.length} environments (${names.join(', ')}) to apply the change now.`
  return {
    title: `${change.serviceName} was ${verb}`,
    description: `${effect}${where}`,
  }
}

export type RedeployOutcome = {
  environment: string
  failed?: string
  /** Why this environment was not redeployed, when it was not. */
  skipped?: string
}

/** The API calls a redeploy makes; injectable so the routing can be tested. */
export type RedeployApi = {
  getDeployment: typeof getDeployment
  triggerProjectPipeline: typeof triggerProjectPipeline
  deployFromImage: typeof deployFromImage
  deployFromStatic: typeof deployFromStatic
}

export const sdkRedeployApi: RedeployApi = {
  getDeployment,
  triggerProjectPipeline,
  deployFromImage,
  deployFromStatic,
}

/**
 * Redeploy one deployment with the same source it was built from. Resolves to
 * the reason it could not be redeployed, or `undefined` once it was started.
 */
export async function redeployDeployment(
  api: RedeployApi,
  projectId: number,
  projectSourceType: SourceType,
  deployment: DeploymentResponse
): Promise<string | undefined> {
  const plan = deploymentRedeployPlan(deployment, projectSourceType)
  const environmentId = deployment.environment_id
  switch (plan.kind) {
    case 'git':
      await api.triggerProjectPipeline({
        path: { id: projectId },
        body: {
          branch: deployment.branch,
          commit: deployment.commit_hash,
          tag: deployment.tag,
          environment_id: environmentId,
        },
        throwOnError: true,
      })
      return undefined
    case 'docker_image': {
      const imageRef = deployment.metadata?.externalImageRef
      if (!imageRef) return 'its image reference is unknown'
      await api.deployFromImage({
        path: { project_id: projectId, environment_id: environmentId },
        body: {
          ...historicalImageRuntime(deployment.metadata),
          image_ref: imageRef,
        },
        throwOnError: true,
      })
      return undefined
    }
    case 'static_files':
      if (!plan.staticBundleId) return 'its static bundle is no longer stored'
      await api.deployFromStatic({
        path: { project_id: projectId, environment_id: environmentId },
        body: {
          static_bundle_id: plan.staticBundleId,
          health_check_path: deployment.metadata?.healthCheckPath,
        },
        throwOnError: true,
      })
      return undefined
    case 'unsupported':
      return plan.sourceType === 'uploaded_source'
        ? 'it was deployed from an uploaded archive; upload it again'
        : 'it has no reusable source to rebuild from'
  }
}

/** How long to wait for the live deployments before offering the action anyway. */
const REDEPLOYABILITY_DEADLINE_MS = 4000

/**
 * What the console can do for the live deployments of `environments`:
 *
 * - `redeploy`: at least one can be redeployed in place (or this cannot be
 *   told, see below).
 * - `upload`: every one was made from an uploaded archive, which the console
 *   cannot rebuild, so the source has to be uploaded again.
 * - `no-source`: every one is a manual or external deployment, which has
 *   nothing to rebuild from and nothing to upload. The redeploy itself says
 *   so; sending the user to an upload, or to a new-deployment dialog, would
 *   hide that.
 *
 * Decided from each environment's current deployment, the way the redeploy
 * itself decides ({@link deploymentRedeployPlan}), not from the project's
 * source type: a project's type is its primary source, not a restriction, so a
 * manual project can run an image or a Git ref, and a Git project can hold an
 * uploaded archive. A mix of the two unredeployable kinds stays `redeploy`,
 * and the redeploy reports each environment on its own.
 *
 * It answers as soon as one environment is known to be redeployable, and gives
 * up waiting after a few seconds. When a deployment cannot be read, there is
 * nothing to decide, or the lookup is too slow, this says `redeploy`: the
 * redeploy itself reports why an environment was skipped, and withholding the
 * action on a failed read would regress working projects.
 */
export type RedeployAvailability = 'redeploy' | 'upload' | 'no-source'

export async function redeployAvailability(
  projectId: number,
  projectSourceType: SourceType,
  environments: EnvironmentResponse[],
  api: Pick<RedeployApi, 'getDeployment'> = sdkRedeployApi,
  deadlineMs = REDEPLOYABILITY_DEADLINE_MS
): Promise<RedeployAvailability> {
  if (environments.length === 0) return 'redeploy'
  const controller = new AbortController()
  return new Promise<RedeployAvailability>((resolve) => {
    let uploads = 0
    let withoutSource = 0
    const finish = (availability: RedeployAvailability) => {
      clearTimeout(timer)
      // Stop reads that are still waiting: the answer is known.
      controller.abort()
      resolve(availability)
    }
    // A lookup that never answers must not hold back the confirmation that
    // depends on it; without an answer the action is offered, as for a failed
    // read.
    const timer = setTimeout(() => finish('redeploy'), deadlineMs)
    for (const environment of environments) {
      void api
        .getDeployment({
          path: {
            project_id: projectId,
            deployment_id: environment.current_deployment_id as number,
          },
          signal: controller.signal,
          throwOnError: true,
        })
        .then(({ data: deployment }) => {
          const plan = deploymentRedeployPlan(deployment, projectSourceType)
          if (plan.kind !== 'unsupported') return finish('redeploy')
          if (plan.sourceType === 'uploaded_source') uploads += 1
          else withoutSource += 1
          if (uploads + withoutSource < environments.length) return
          if (withoutSource === 0) finish('upload')
          else if (uploads === 0) finish('no-source')
          else finish('redeploy')
        })
        .catch(() => finish('redeploy'))
    }
  })
}

/**
 * Whether the console should offer to redeploy rather than send the user to
 * upload the source again: false only when every live deployment was made
 * from an uploaded archive. See {@link redeployAvailability}.
 */
export async function canRedeployAnyInPlace(
  projectId: number,
  projectSourceType: SourceType,
  environments: EnvironmentResponse[],
  api: Pick<RedeployApi, 'getDeployment'> = sdkRedeployApi,
  deadlineMs = REDEPLOYABILITY_DEADLINE_MS
): Promise<boolean> {
  return (
    (await redeployAvailability(
      projectId,
      projectSourceType,
      environments,
      api,
      deadlineMs
    )) !== 'upload'
  )
}

/**
 * Redeploy the current deployment of every environment returned by
 * {@link environmentsToRedeploy}, each from its own source (commit, image or
 * static bundle), so only the environment variables change. Reports each API failure without losing successful outcomes; environments that cannot be redeployed are reported as
 * skipped rather than failing the rest.
 */
export async function redeployCurrentDeployments(
  projectId: number,
  projectSourceType: SourceType,
  environments: EnvironmentResponse[],
  api: RedeployApi = sdkRedeployApi,
  completed: Set<number> = new Set()
): Promise<RedeployOutcome[]> {
  const outcomes: RedeployOutcome[] = []
  for (const environment of environmentsToRedeploy(environments)) {
    if (completed.has(environment.id)) continue
    try {
      const { data: deployment } = await api.getDeployment({
        path: {
          project_id: projectId,
          deployment_id: environment.current_deployment_id as number,
        },
        throwOnError: true,
      })
      const skipped = await redeployDeployment(
        api,
        projectId,
        projectSourceType,
        deployment
      )
      outcomes.push({
        environment: environment.name,
        skipped,
      })
      if (!skipped) completed.add(environment.id)
    } catch (error) {
      const failed =
        (error as { detail?: string })?.detail ??
        (error instanceof Error ? error.message : 'Request failed')
      outcomes.push({
        environment: environment.name,
        failed,
      })
    }
  }
  return outcomes
}

/** Fetch a project's environments for {@link environmentsToRedeploy}. */
export async function fetchProjectEnvironments(
  projectId: number
): Promise<EnvironmentResponse[]> {
  const { data } = await getEnvironments({
    path: { project_id: projectId },
    throwOnError: true,
  })
  return data
}

/** One line summarising a redeploy for a toast. */
export function redeploySummary(outcomes: RedeployOutcome[]): {
  started: number
  message: string
} {
  const started = outcomes.filter(
    (outcome) => !outcome.skipped && !outcome.failed
  )
  const skipped = outcomes.filter((outcome) => outcome.skipped)
  const parts: string[] = outcomes
    .filter((o) => o.failed)
    .map((o) => `${o.environment} redeploy failed: ${o.failed}`)
  if (started.length > 0) {
    parts.push(
      started.length === 1
        ? `Redeploying ${started[0].environment}`
        : `Redeploying ${started.length} environments`
    )
  }
  for (const outcome of skipped) {
    parts.push(`${outcome.environment} not redeployed: ${outcome.skipped}`)
  }
  return {
    started: started.length,
    message: parts.join('. ') || 'Nothing to redeploy',
  }
}

/**
 * Toast variant of the redeploy prompt, for places where a link changes on
 * a page that is not the project's (the service's own page). Says nothing
 * extra when the project has nothing deployed yet.
 */
export async function toastRedeployAfterLinkChange(
  project: { id: number; name: string; source_type: SourceType },
  change: ServiceLinkChange,
  notify: {
    info: (
      title: string,
      options: {
        description: string
        duration: number
        action?: { label: string; onClick: () => void }
      }
    ) => void
    success: (message: string) => void
    warning: (message: string) => void
    error: (message: string) => void
  }
): Promise<void> {
  let environments: EnvironmentResponse[]
  try {
    environments = await fetchProjectEnvironments(project.id)
  } catch {
    notify.warning(
      `${project.name}: the service was ${change.kind}. Running apps retain their old connection variables until redeployed. Open the project's deployments to apply the change.`
    )
    return
  }
  const targets = environmentsToRedeploy(environments)
  if (targets.length === 0) return
  const redeployable = await canRedeployAnyInPlace(
    project.id,
    project.source_type,
    targets
  )
  const { description } = serviceLinkRedeployMessage(
    change,
    targets,
    redeployable
  )
  if (!redeployable) {
    // Nothing the toast could start: the source has to be uploaded again from
    // the project, so it only says so.
    notify.info(`${project.name}: upload the source to apply`, {
      description,
      duration: 20_000,
    })
    return
  }
  const completed = new Set<number>()
  notify.info(`${project.name}: redeploy to apply`, {
    description,
    duration: 20_000,
    action: {
      label: 'Redeploy now',
      onClick: () => {
        redeployCurrentDeployments(
          project.id,
          project.source_type,
          environments,
          sdkRedeployApi,
          completed
        )
          .then((outcomes) => {
            const summary = redeploySummary(outcomes)
            if (summary.started > 0) notify.success(summary.message)
            else notify.warning(summary.message)
          })
          .catch((error: unknown) =>
            notify.error(
              `Failed to start the redeploy of ${project.name}: ${
                (error as { detail?: string })?.detail ??
                (error instanceof Error ? error.message : 'unknown error')
              }`
            )
          )
      },
    },
  })
}
