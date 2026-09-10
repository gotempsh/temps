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
  const hasGitData = Boolean(
    deployment.branch || deployment.commit_hash || deployment.commit_message
  )
  const sourceType =
    metadata?.deploymentSourceType ?? (hasGitData ? 'git' : projectSourceType)

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
