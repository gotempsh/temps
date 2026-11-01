// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defaultDeployEnvironment } from '@/lib/project-deploy-action'
import { isActiveDeploymentStatus } from '@/lib/recent-deployments'
import type { ReachabilityHeadline } from '@/lib/reachability'

/** URL search parameter that carries the selected environment's slug. */
export const ENVIRONMENT_PARAM = 'env'

/** Value of {@link ENVIRONMENT_PARAM} that selects every environment. */
export const ALL_ENVIRONMENTS = 'all'

/**
 * `?environment=` from links made before the project bar (alarms, Shield,
 * reachability, older bookmarks): an environment id, or sometimes a slug.
 */
export const LEGACY_ENVIRONMENT_PARAM = 'environment'

/** The slug a legacy `?environment=` value names, if it names one. */
export function legacyEnvironmentSlug(
  environments: ReadonlyArray<{ id: number; slug: string }>,
  value: string | null
): string | null {
  if (!value) return null
  const match = environments.find(
    (environment) =>
      String(environment.id) === value || environment.slug === value
  )
  return match?.slug ?? null
}

interface SelectableEnvironment {
  id: number
  slug: string
  is_preview: boolean
}

export type EnvironmentSelectionSource =
  /** `?env=` in the URL. */
  | 'url'
  /** The last environment picked for this project in this browser. */
  | 'remembered'
  /** The environment marked as this project's default. */
  | 'default'
  /** Production, else the first non-preview environment. */
  | 'fallback'

export interface EnvironmentSelection<T extends SelectableEnvironment> {
  /** The environment the page shows; undefined only when there is none. */
  environment: T | undefined
  /** True when the page shows every environment ("All environments"). */
  all: boolean
  source: EnvironmentSelectionSource
}

function bySlug<T extends SelectableEnvironment>(
  environments: readonly T[],
  slug: string | null | undefined
): T | undefined {
  return slug ? environments.find((env) => env.slug === slug) : undefined
}

/**
 * Which environment a project page shows.
 *
 * Order: the URL, then the environment last picked for this project, then the
 * project's default, then production. A slug that no longer exists (renamed or
 * deleted environment) is skipped rather than shown as an empty page.
 * "All environments" only applies where the page can list several; elsewhere
 * it falls through to the next candidate.
 */
export function resolveEnvironmentSelection<T extends SelectableEnvironment>({
  environments,
  urlParam,
  remembered,
  preferredDefault,
  allowAll,
}: {
  environments: readonly T[]
  urlParam: string | null
  remembered: string | null
  preferredDefault: string | null
  allowAll: boolean
}): EnvironmentSelection<T> {
  const fallback = defaultDeployEnvironment(environments)
  const candidates: [string | null, EnvironmentSelectionSource][] = [
    [urlParam, 'url'],
    [remembered, 'remembered'],
  ]
  for (const [value, source] of candidates) {
    if (value === ALL_ENVIRONMENTS && allowAll)
      return { environment: undefined, all: true, source }
    const match = bySlug(environments, value)
    if (match) return { environment: match, all: false, source }
  }
  const preferred = bySlug(environments, preferredDefault)
  if (preferred)
    return { environment: preferred, all: false, source: 'default' }
  return { environment: fallback, all: false, source: 'fallback' }
}

/** The environment new visitors see: the stored default, else production. */
export function resolveDefaultEnvironment<T extends SelectableEnvironment>(
  environments: readonly T[],
  preferredDefault: string | null
): T | undefined {
  return (
    bySlug(environments, preferredDefault) ??
    defaultDeployEnvironment(environments)
  )
}

export interface EnvironmentLiveStatus {
  tone: 'ok' | 'idle' | 'running' | 'warn' | 'error'
  label: string
  /** What the state means: the tooltip, and what screen readers announce. */
  detail: string
  /** Where a problem is explained and fixed. Set for problem states only. */
  explainedAt?: 'deployment' | 'environment' | 'monitors' | 'reachability'
}

/** What else is known about an environment, beyond its own record. */
export interface EnvironmentSignals {
  /** The newest deployment *of this environment*, whatever its outcome. */
  latestDeployment?: { status: string } | null
  /** Live containers of this environment that are down or crash-looping. */
  unhealthyContainers?: number
  /**
   * The uptime monitors' verdict (`operational`, `degraded`, `down`, …).
   * Monitors are per project, so pass it only for the environment they
   * watch: the one the project treats as production.
   */
  monitorStatus?: string
}

/**
 * The environment bar's one-word state. A problem outranks "Live": a failed
 * latest deployment reads "Failed" even while an older version serves, and
 * crashing containers or failing monitors read "Degraded" or "Down", each
 * pointing at where the problem is explained.
 */
export function environmentLiveStatus(
  environment: { sleeping: boolean; current_deployment_id?: number | null },
  signals: EnvironmentSignals = {}
): EnvironmentLiveStatus {
  const latest = signals.latestDeployment?.status
  const building = !!latest && isActiveDeploymentStatus(latest)
  const live = !!environment.current_deployment_id
  if (latest === 'failed')
    return {
      tone: 'error',
      label: 'Failed',
      detail: live
        ? 'The latest deployment failed; the previous version is still serving. Open it for the reason.'
        : 'The first deployment failed, so nothing is live yet. Open it for the reason.',
      explainedAt: 'deployment',
    }
  if (building)
    return {
      tone: 'running',
      label: 'Deploying',
      detail: live
        ? 'A new version is being deployed; the current one serves until it is replaced.'
        : 'The first deployment is running.',
      explainedAt: 'deployment',
    }
  if (!live)
    return {
      tone: 'idle',
      label: 'Not deployed',
      detail: 'Nothing has been deployed to this environment yet.',
    }
  if (environment.sleeping)
    return {
      tone: 'idle',
      label: 'Sleeping',
      detail: 'Stopped while idle; the next request wakes it up.',
    }
  const unhealthy = signals.unhealthyContainers ?? 0
  if (unhealthy > 0)
    return {
      tone: 'warn',
      label: 'Degraded',
      detail: `${unhealthy} live ${unhealthy === 1 ? 'container is' : 'containers are'} down or restarting.`,
      explainedAt: 'environment',
    }
  if (signals.monitorStatus === 'down')
    return {
      tone: 'error',
      label: 'Down',
      detail: 'Uptime monitors cannot reach this service.',
      explainedAt: 'monitors',
    }
  if (signals.monitorStatus === 'degraded')
    return {
      tone: 'warn',
      label: 'Degraded',
      detail: 'Some uptime monitors are failing.',
      explainedAt: 'monitors',
    }
  return {
    tone: 'ok',
    label: 'Live',
    detail:
      signals.monitorStatus === 'operational'
        ? 'Serving its current deployment; uptime monitors are passing.'
        : 'Serving its current deployment.',
  }
}

/**
 * Folds where visitors reach a live environment into its status, so the
 * project bar says it once: "Live" with the address in its detail, or "Not
 * reachable" with the reason, which opens the page that fixes it. Any other
 * state (failed, deploying, down) already says more and is left alone.
 */
export function withReachability(
  status: EnvironmentLiveStatus,
  headline: ReachabilityHeadline | undefined
): EnvironmentLiveStatus {
  if (status.tone !== 'ok' || !headline) return status
  switch (headline.kind) {
    case 'public':
      return { ...status, detail: `Public at ${headline.address}.` }
    case 'private':
      return {
        ...status,
        detail: `Private at ${headline.address}${headline.network ? ` (${headline.network})` : ''}.`,
      }
    case 'not_reachable':
      return {
        tone: 'warn',
        label: 'Not reachable',
        detail: headline.reason,
        explainedAt: 'reachability',
      }
    case 'checking':
    case 'unknown':
      return status
  }
}
