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
  })

  test('maps shared platform routes', () => {
    expect(featureKeyForPath('/monitoring/alerts')).toBe('alerts-metric-alerts')
    expect(featureKeyForPath('/settings/nodes')).toBe('multi-node-worker-join')
    expect(featureKeyForPath('/ai-gateway/activity')).toBe('ai-gateway')
  })

  test('does not label stable or unresolved surfaces', () => {
    expect(featureKeyForPath('/projects')).toBeUndefined()
    expect(featureKeyForPath('/mcp-servers')).toBeUndefined()
    expect(featureKeyForPath('/domains')).toBeUndefined()
  })
})
