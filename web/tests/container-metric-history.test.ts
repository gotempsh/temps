import { describe, expect, test } from 'bun:test'
import { buildMetricHistorySeries } from '../src/components/containers/container-metric-history'

describe('buildMetricHistorySeries', () => {
  test('appends the live sample to stored history', () => {
    expect(buildMetricHistorySeries([{ value: 1 }, { value: 2 }], 3)).toEqual([
      1, 2, 3,
    ])
  })

  test('uses the live sample when stored history is unavailable', () => {
    expect(buildMetricHistorySeries(undefined, 12)).toEqual([12])
  })

  test('filters non-finite samples so the chart remains renderable', () => {
    expect(
      buildMetricHistorySeries(
        [
          { value: 1 },
          { value: Number.NaN },
          { value: Number.POSITIVE_INFINITY },
        ],
        Number.NEGATIVE_INFINITY
      )
    ).toEqual([1])
  })
})
