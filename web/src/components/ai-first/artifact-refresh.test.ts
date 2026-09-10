// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { shouldRefreshArtifactsForLiveEvent } from './artifact-refresh'

describe('artifact live invalidation', () => {
  test('refreshes only for artifact changes and bounded recovery events', () => {
    expect(shouldRefreshArtifactsForLiveEvent('artifacts_changed')).toBe(true)
    expect(shouldRefreshArtifactsForLiveEvent('turn_complete')).toBe(true)
    expect(shouldRefreshArtifactsForLiveEvent('resync_required')).toBe(true)
    expect(shouldRefreshArtifactsForLiveEvent('token')).toBe(false)
    expect(shouldRefreshArtifactsForLiveEvent('tool_call')).toBe(false)
    expect(shouldRefreshArtifactsForLiveEvent('turn_state')).toBe(false)
  })
})
