// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceType } from '@/api/client'

interface DeploymentComposeRevision {
  metadata?: {
    sourceBundleId?: number | null
  } | null
}

export function composeRevisionForRedeploy(
  deployment: DeploymentComposeRevision | null | undefined
): number | undefined {
  return deployment?.metadata?.sourceBundleId ?? undefined
}

export function projectDeployLaunchMode(
  sourceType: SourceType
): 'dialog' | 'upload' {
  return sourceType === 'uploaded_source' || sourceType === 'static_files'
    ? 'upload'
    : 'dialog'
}

export function deploymentsAfterStartPath(projectSlug: string): string {
  return `/projects/${projectSlug}/deployments?autoRefresh=true`
}
