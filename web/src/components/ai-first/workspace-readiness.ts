// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ApplicationWorkspaceResponse,
  ProviderCatalogDto,
} from '@/api/client'

export type WorkspaceHarnessOption = {
  id: string
  name: string
  authMethod: string | null
  models: Array<{ id: string; name: string }>
}

export function workspaceHarnessOptions(
  providers: ProviderCatalogDto[]
): WorkspaceHarnessOption[] {
  return providers
    .filter((provider) => provider.workspace_ready)
    .map((provider) => ({
      id: provider.id,
      name: provider.name,
      authMethod: provider.current_auth_type ?? null,
      models: provider.runtime_models,
    }))
}

export type WorkspaceStatusPresentation = {
  label: string
  detail: string
  dot: string
}

export function workspaceStatusClickTarget(
  hasApplication: boolean,
  workspace: ApplicationWorkspaceResponse | null
): 'workspace' | null {
  return hasApplication || workspace ? 'workspace' : null
}

export function workspaceStatusPresentation(
  workspace: ApplicationWorkspaceResponse | null,
  loading: boolean,
  waking = false
): WorkspaceStatusPresentation {
  if (waking) {
    return {
      label: 'Sandbox waking',
      detail: workspace?.sandbox_public_id
        ? `${workspace.sandbox_public_id} is being resumed and checked for accessibility`
        : 'The persistent sandbox is being started',
      dot: 'animate-pulse bg-amber-500',
    }
  }
  if (loading && !workspace) {
    return {
      label: 'Sandbox checking',
      detail: 'Checking sandbox accessibility',
      dot: 'animate-pulse bg-amber-500',
    }
  }
  if (!workspace || workspace.state === 'failed') {
    return {
      label: 'Sandbox unavailable',
      detail: workspace?.last_error ?? 'Sandbox status could not be confirmed',
      dot: 'bg-red-500',
    }
  }
  if (workspace.state === 'running' && workspace.persistent_volume_healthy) {
    return {
      label: 'Sandbox ready',
      detail: workspace.sandbox_public_id
        ? `${workspace.sandbox_public_id} is running and accessible`
        : 'Sandbox is running and accessible',
      dot: 'bg-emerald-500',
    }
  }
  if (workspace.state === 'running') {
    return {
      label: 'Sandbox unavailable',
      detail:
        'Sandbox compute is running, but its persistent volume is unavailable',
      dot: 'bg-red-500',
    }
  }
  if (workspace.state === 'recovering') {
    return {
      label: 'Sandbox starting',
      detail: workspace.sandbox_public_id
        ? `${workspace.sandbox_public_id} is recovering`
        : 'Sandbox is being created',
      dot: 'animate-pulse bg-amber-500',
    }
  }
  return {
    label: 'Sandbox sleeping',
    detail: workspace.sandbox_public_id
      ? `${workspace.sandbox_public_id} is not currently accessible`
      : 'No sandbox has been started for this workspace yet',
    dot: 'bg-red-500',
  }
}
