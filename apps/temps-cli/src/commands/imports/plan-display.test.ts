import { test, expect, describe } from 'bun:test'
import { riskBadge } from './plan-display.js'
import type { RiskLevel } from '../../api/types.gen.js'

describe('riskBadge', () => {
  test('labels each known risk level', () => {
    const expected: Record<RiskLevel, string> = {
      none: 'NONE',
      low: 'LOW',
      medium: 'MEDIUM',
      high: 'HIGH',
      critical: 'CRITICAL',
    }
    for (const [risk, label] of Object.entries(expected) as [RiskLevel, string][]) {
      expect(riskBadge(risk)).toContain(label)
    }
  })

  test('falls back to echoing the raw value for an unrecognized risk', () => {
    expect(riskBadge('weird' as RiskLevel)).toContain('weird')
  })
})
