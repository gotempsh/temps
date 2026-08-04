import { describe, expect, test } from 'bun:test'
import { buildProjectSetupSteps } from './useProjectSetup'

describe('buildProjectSetupSteps', () => {
  test('builds every project-scoped setup destination', () => {
    const steps = buildProjectSetupSteps('storefront', {
      hasAnalyticsEvents: false,
      hasErrorGroups: false,
      customDomainCount: 0,
      monitorCount: 0,
      serviceCount: 0,
      hasTelemetryTraces: false,
    })

    expect(steps.map((step) => step.id)).toEqual([
      'analytics',
      'errors',
      'domain',
      'monitoring',
      'storage',
      'telemetry',
    ])
    expect(steps.map((step) => step.href)).toEqual([
      '/projects/storefront/analytics/setup',
      '/projects/storefront/errors/setup',
      '/projects/storefront/domains',
      '/projects/storefront/monitors',
      '/projects/storefront/storage',
      '/projects/storefront/traces#traces-setup',
    ])
  })

  test('derives completion from project activation signals', () => {
    const steps = buildProjectSetupSteps('storefront', {
      hasAnalyticsEvents: true,
      hasErrorGroups: false,
      customDomainCount: 2,
      monitorCount: 1,
      serviceCount: 0,
      hasTelemetryTraces: true,
    })

    expect(steps.filter((step) => step.done).map((step) => step.id)).toEqual([
      'analytics',
      'domain',
      'monitoring',
      'telemetry',
    ])
  })
})
