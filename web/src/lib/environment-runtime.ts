// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  BuildLocation,
  EnvironmentResponse,
  ProjectResponse,
} from '@/api/client'

const BUILDS_ON: Record<BuildLocation, string> = {
  control_plane: 'the control plane',
  node: 'a worker node',
  ci: 'CI',
}

function formatCpu(cores: number | null | undefined) {
  return cores ? `${cores} ${cores === 1 ? 'core' : 'cores'}` : 'No limit'
}

function formatMemory(mb: number | null | undefined) {
  if (!mb) return 'No limit'
  return mb >= 1024 && mb % 1024 === 0 ? `${mb / 1024} GB` : `${mb} MB`
}

/**
 * What an environment runs, as one line: branch and trigger, then size and
 * where it builds. Replicas, resources and the branch are per environment,
 * so they are edited there; this is where someone looking for them under
 * "Build & deploy" finds them.
 */
export function environmentRuntimeFacts(
  project: ProjectResponse,
  environment: EnvironmentResponse
): { branch: string; trigger: string; details: string[] } {
  const config = environment.deployment_config
  const automatic =
    config?.automaticDeploy ?? project.deployment_config.automaticDeploy
  const replicas = config?.replicas ?? 1
  const cpu = formatCpu(config?.cpuLimit)
  const memory = formatMemory(config?.memoryLimit)
  return {
    branch: environment.branch || project.main_branch || 'no branch set',
    trigger: automatic ? 'on every push' : 'manually',
    details: [
      `${replicas} ${replicas === 1 ? 'replica' : 'replicas'}`,
      cpu === 'No limit' ? 'no CPU limit' : cpu,
      memory === 'No limit' ? 'no memory limit' : `${memory} memory`,
      `builds on ${BUILDS_ON[config?.buildLocation ?? 'control_plane']}`,
    ],
  }
}
