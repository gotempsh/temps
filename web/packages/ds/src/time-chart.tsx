// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Reuses the recharts internals of `ThresholdLineChart`
// (web/src/components/charts/threshold-line-chart.tsx) rather than
// rewriting the charting engine — that component already handles
// thresholds, shaded bands, deployment markers, and drag-to-select ranges,
// which every one of the 14 hand-rolled stat-tile/chart-panel call sites
// listed in the handoff doc re-implements piecemeal.
export {
  ThresholdLineChart as TimeChart,
  type ThresholdLineSeries as TimeChartSeries,
  type ThresholdBand as TimeChartThreshold,
  type ThresholdBandArea as TimeChartBand,
  type ThresholdMarker as TimeChartMarker,
} from '../../../src/components/charts/threshold-line-chart'
