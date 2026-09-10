// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'

export function deploymentSourceLabel(
  deployment: Pick<DeploymentResponse, 'branch' | 'metadata'>
): string {
  if (deployment.branch) return deployment.branch

  switch (deployment.metadata?.deploymentSourceType) {
    case 'docker_image':
      return 'Docker image'
    case 'static_files':
      return 'Static files'
    case 'uploaded_source':
      return 'Uploaded source'
    default:
      return 'Manual source'
  }
}
