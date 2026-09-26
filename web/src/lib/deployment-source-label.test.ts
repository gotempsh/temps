// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { deploymentSourceLabel } from './deployment-source-label'

describe('deploymentSourceLabel', () => {
  it('uses an explicit branch for Git deployments', () => {
    expect(deploymentSourceLabel({ branch: 'main', metadata: null })).toBe(
      'main'
    )
  })

  it('labels uploaded archives', () => {
    expect(
      deploymentSourceLabel({
        branch: null,
        metadata: { deploymentSourceType: 'uploaded_source' },
      })
    ).toBe('Uploaded source')
  })
})
