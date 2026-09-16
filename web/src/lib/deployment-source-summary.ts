// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse, SourceType } from '@/api/client'

export type DeploymentSourceSummary =
  | {
      kind: 'git'
      branch?: string
      commit?: string
      message?: string
    }
  | {
      kind: 'docker_image' | 'static_files' | 'uploaded_source' | 'manual'
      label: string
      detail?: string
    }

export type DeploymentRedeployPlan =
  | { kind: 'git' }
  | { kind: 'docker_image' }
  | { kind: 'static_files'; staticBundleId?: number }
  | { kind: 'unsupported'; sourceType: 'uploaded_source' | 'manual' }

export function resolveDeploymentSourceType(
  deployment: DeploymentResponse,
  projectSourceType?: SourceType
): SourceType | undefined {
  const metadata = deployment.metadata
  if (metadata?.deploymentSourceType) return metadata.deploymentSourceType

  const hasGitData = Boolean(
    deployment.branch || deployment.commit_hash || deployment.commit_message
  )
  if (hasGitData) return 'git'

  if (metadata?.externalImageRef || metadata?.uploadedImageId) {
    return 'docker_image'
  }

  return projectSourceType
}

/**
 * Selects the only API contract that may be used to redeploy this historical
 * deployment. Keeping this exhaustive prevents non-Git sources from silently
 * falling through to the branch/commit/tag pipeline.
 */
export function deploymentRedeployPlan(
  deployment: DeploymentResponse,
  projectSourceType: SourceType
): DeploymentRedeployPlan {
  const sourceType =
    resolveDeploymentSourceType(deployment, projectSourceType) ??
    projectSourceType

  switch (sourceType) {
    case 'git':
      return { kind: 'git' }
    case 'docker_image':
      return { kind: 'docker_image' }
    case 'static_files':
      return {
        kind: 'static_files',
        staticBundleId: deployment.metadata?.staticBundleId ?? undefined,
      }
    case 'uploaded_source':
    case 'manual':
      return { kind: 'unsupported', sourceType }
  }
}

function archiveLabel(
  prefix: string,
  contentType?: string | null,
  path?: string | null
): string {
  const format = `${contentType ?? ''} ${path ?? ''}`.toLowerCase()
  if (format.includes('zip')) return `${prefix} (ZIP)`
  if (
    format.includes('gzip') ||
    format.includes('.tgz') ||
    format.includes('.tar.gz')
  ) {
    return `${prefix} (tar.gz)`
  }
  return prefix
}

export function deploymentSourceSummary(
  deployment: DeploymentResponse,
  projectSourceType?: SourceType
): DeploymentSourceSummary {
  const metadata = deployment.metadata
  const sourceType = resolveDeploymentSourceType(deployment, projectSourceType)

  if (sourceType === 'git') {
    return {
      kind: 'git',
      branch: deployment.branch || undefined,
      commit: deployment.commit_hash || undefined,
      message: deployment.commit_message || undefined,
    }
  }

  if (sourceType === 'docker_image') {
    return {
      kind: 'docker_image',
      label: 'Docker image',
      detail:
        metadata?.externalImageRef || metadata?.uploadedImageId || undefined,
    }
  }

  if (sourceType === 'static_files') {
    return {
      kind: 'static_files',
      label: archiveLabel(
        'Static bundle',
        metadata?.staticBundleContentType,
        metadata?.staticBundlePath
      ),
    }
  }

  if (sourceType === 'uploaded_source') {
    return {
      kind: 'uploaded_source',
      label: archiveLabel(
        'Uploaded source',
        metadata?.sourceBundleContentType,
        metadata?.sourceBundlePath
      ),
    }
  }

  return {
    kind: 'manual',
    label: sourceType === 'manual' ? 'Manual deployment' : 'Deployment source',
  }
}
