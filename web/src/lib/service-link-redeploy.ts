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

/** The prompt shown after a link change, e.g. "Postgres was linked. …". */
export function serviceLinkRedeployMessage(
  change: ServiceLinkChange,
  environments: Pick<EnvironmentResponse, 'name'>[]
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

const sdkRedeployApi: RedeployApi = {
  getDeployment,
  triggerProjectPipeline,
  deployFromImage,
  deployFromStatic,
}

/** Redeploy one deployment with the same source it was built from. */
async function redeployDeployment(
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
        action: { label: string; onClick: () => void }
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
  const { description } = serviceLinkRedeployMessage(change, targets)
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
