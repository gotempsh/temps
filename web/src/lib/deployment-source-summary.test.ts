// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse } from '@/api/client'
import { deploymentSourceSummary } from './deployment-source-summary'

function deployment(
  overrides: Partial<DeploymentResponse> = {}
): DeploymentResponse {
  return {
    created_at: 0,
    environment: { domains: [], id: 1, name: 'production', slug: 'production' },
    environment_id: 1,
    id: 1,
    is_current: false,
    project_id: 1,
    status: 'completed',
    url: '',
    ...overrides,
  }
}

describe('deploymentSourceSummary', () => {
  test('keeps only populated Git fields', () => {
    expect(
      deploymentSourceSummary(
        deployment({ branch: 'main', commit_hash: null }),
        'git'
      )
    ).toEqual({
      kind: 'git',
      branch: 'main',
      commit: undefined,
      message: undefined,
    })
  })

  test('shows the image reference instead of empty Git metadata', () => {
    expect(
      deploymentSourceSummary(
        deployment({
          metadata: {
            deploymentSourceType: 'docker_image',
            externalImageRef: 'ghcr.io/example/service:latest',
          },
        }),
        'git'
      )
    ).toEqual({
      kind: 'docker_image',
      label: 'Docker image',
      detail: 'ghcr.io/example/service:latest',
    })
  })

  test('describes uploaded ZIP source without Git placeholders', () => {
    expect(
      deploymentSourceSummary(
        deployment({
          metadata: {
            deploymentSourceType: 'uploaded_source',
            sourceBundleContentType: 'application/zip',
          },
        })
      )
    ).toEqual({
      kind: 'uploaded_source',
      label: 'Uploaded source (ZIP)',
    })
  })

  test('falls back to the project source for older deployment metadata', () => {
    expect(deploymentSourceSummary(deployment(), 'static_files')).toEqual({
      kind: 'static_files',
      label: 'Static bundle',
    })
  })

  test('keeps historical Git deployments Git-labelled after a source change', () => {
    expect(
      deploymentSourceSummary(
        deployment({
          branch: 'main',
          commit_hash: 'abcdef1234567890',
          commit_message: 'Initial deployment',
        }),
        'docker_image'
      )
    ).toEqual({
      kind: 'git',
      branch: 'main',
      commit: 'abcdef1234567890',
      message: 'Initial deployment',
    })
  })

  test('does not expose internal static bundle paths', () => {
    expect(
      deploymentSourceSummary(
        deployment({
          metadata: {
            deploymentSourceType: 'static_files',
            staticBundlePath: 'internal/blobs/project-42/site.tar.gz',
          },
        })
      )
    ).toEqual({
      kind: 'static_files',
      label: 'Static bundle (tar.gz)',
    })
  })
})
