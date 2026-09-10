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

export function deploymentsAfterStartPath(projectSlug: string): string {
  return `/projects/${projectSlug}/deployments?autoRefresh=true`
}
