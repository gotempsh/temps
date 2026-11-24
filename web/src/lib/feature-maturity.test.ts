// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { featureKeyForPath } from './feature-maturity'

describe('feature maturity route mapping', () => {
  test('uses the most specific project analytics feature', () => {
    expect(featureKeyForPath('/projects/shop/analytics')).toBe('web-analytics')
    expect(featureKeyForPath('/projects/shop/analytics/replays')).toBe(
      'session-replay'
    )
    expect(featureKeyForPath('/projects/shop/analytics/funnels/12')).toBe(
      'funnels'
    )
    expect(featureKeyForPath('/projects/shop/analytics/surveys/3')).toBe(
      'surveys'
    )
  })

  test('maps shared platform routes', () => {
    expect(featureKeyForPath('/monitoring/alerts')).toBe('alerts-metric-alerts')
    expect(featureKeyForPath('/settings/nodes')).toBe('multi-node-worker-join')
    expect(featureKeyForPath('/ai-gateway/activity')).toBe('ai-gateway')
  })

  test('labels the assistant and gateway pages, and nothing for retired AI routes', () => {
    expect(featureKeyForPath('/chat')).toBe('ai-chat')
    expect(featureKeyForPath('/ai-gateway/limits')).toBe('ai-gateway')
    expect(featureKeyForPath('/ai-gateway/models')).toBe('ai-gateway')
    // Harnesses, the harness workspace and workflows were retired; their old
    // URLs only redirect, so they carry no maturity of their own.
    expect(featureKeyForPath('/ai-first')).toBeUndefined()
    expect(featureKeyForPath('/ai-workflows')).toBeUndefined()
    expect(featureKeyForPath('/agent-sandbox/providers')).toBeUndefined()
    expect(featureKeyForPath('/projects/shop/agents')).toBeUndefined()
  })

  test('does not label stable or unresolved surfaces', () => {
    expect(featureKeyForPath('/projects')).toBeUndefined()
    expect(featureKeyForPath('/mcp-servers')).toBeUndefined()
    expect(featureKeyForPath('/domains')).toBeUndefined()
  })
})
