// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  DeploymentFailureInfo,
  DeploymentFailureStage,
  FailureSettingsSection,
} from '@/api/client'

const STAGE_LABELS: Record<DeploymentFailureStage, string> = {
  source: 'Source',
  configuration: 'Configuration',
  dependency_install: 'Dependency install',
  build: 'Build',
  image: 'Image',
  deploy: 'Deploy',
  runtime: 'Runtime',
  health_check: 'Health check',
  resource: 'Resources',
  platform: 'Platform',
  unknown: 'Unknown',
}

export function failureStageLabel(stage: DeploymentFailureStage): string {
  return STAGE_LABELS[stage] ?? 'Unknown'
}

export interface FailureSettingsLink {
  href: string
  label: string
}

/**
 * Deep link to the settings surface that fixes a classified failure. Project
 * sections are relative to the project; registry and build limits are
 * instance-wide pages.
 */
export function failureSettingsLink(
  section: FailureSettingsSection | null | undefined,
  projectSlug: string
): FailureSettingsLink | null {
  if (!section) return null
  const project = `/projects/${projectSlug}/settings`
  switch (section) {
    case 'source':
      return { href: `${project}/build?tab=source`, label: 'Source settings' }
    case 'build':
      return { href: `${project}/build?tab=build`, label: 'Build settings' }
    case 'deploy':
      return {
        href: `${project}/build?tab=deploy`,
        label: 'Deployment settings',
      }
    case 'environment_variables':
      return {
        href: `${project}/environment-variables`,
        label: 'Environment variables',
      }
    case 'git':
      return { href: `${project}/git`, label: 'Git settings' }
    case 'docker_registry':
      return { href: '/settings/docker-registry', label: 'Docker registries' }
    case 'build_limits':
      return { href: '/settings/build-limits', label: 'Build limits' }
    default:
      return null
  }
}

function formatSeconds(seconds: number): string {
  if (seconds < 120) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  const rest = seconds % 60
  return rest === 0 ? `${minutes} min` : `${minutes} min ${rest}s`
}

/**
 * One-line description of a timeout's limit and runtime, e.g.
 * "Limit 5 min · ran 5 min 3s". Null when the failure is not a timeout or the
 * reason did not state the numbers.
 */
export function failureTimeoutSummary(
  failure: Pick<
    DeploymentFailureInfo,
    'timeout_limit_seconds' | 'timeout_elapsed_seconds'
  >
): string | null {
  const parts: string[] = []
  if (failure.timeout_limit_seconds != null) {
    parts.push(`Limit ${formatSeconds(failure.timeout_limit_seconds)}`)
  }
  if (failure.timeout_elapsed_seconds != null) {
    parts.push(`ran ${formatSeconds(failure.timeout_elapsed_seconds)}`)
  }
  return parts.length > 0 ? parts.join(' · ') : null
}
