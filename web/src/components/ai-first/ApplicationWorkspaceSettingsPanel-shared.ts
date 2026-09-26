// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ApplicationWorkspaceResponse } from '@/api/client'

export type Props = {
  layout?: 'panel' | 'page'
  applicationPublicId: string
  initialWorkspace?: ApplicationWorkspaceResponse | null
  onWorkspaceChange?: (workspace: ApplicationWorkspaceResponse) => void
  waking?: boolean
}

export function workspaceResourceFingerprint(
  workspace: ApplicationWorkspaceResponse
) {
  return JSON.stringify([
    workspace.runtime,
    workspace.cpu_limit,
    workspace.memory_limit_mb,
    workspace.pids_limit,
    workspace.disk_limit_mb,
    workspace.idle_timeout_secs,
  ])
}

export type ResourceForm = {
  runtime: string
  cpu_limit: string
  memory_limit_mb: string
  pids_limit: string
  disk_limit_mb: string
  idle_timeout_secs: string
}
