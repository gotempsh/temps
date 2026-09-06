// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Operator component library ("op"). These are the components the three
 * page templates are built from, and the ones a new console screen should
 * reach for first. See docs/design-system-handoff.md §6.
 */
export { Kbd, MOD, IS_MAC } from './kbd'
export { Status, StatusLine, AttentionHost, Phrase, worst, GLYPH, GLYPH_CLASS, STATE_RANK, type State, type StatusItem } from './status'
export { Num, Metric, MetricGrid } from './num'
export { fmtNum, fmtPct, fmtBytes, fmtDuration, fmtRelative, fmtAbsolute, fmtCount, EMPTY, type Locale } from './fmt'
export { PageState, type PageStateProps } from './page-state'
export { EchoDialog } from './echo-dialog'
export { Ledger, Detail, Settings, Field, Segmented, PageTitle, Pager, ActionBar, SectionTitle, Section,
  Columns, Lede, KeyValue, Timeline, type KV, type TimelineItem, PAGE_SIZES, type Page, type Crumb, type LedgerRow, type LedgerColumn, type LedgerSort } from './templates'
export { Picker, type PickerOption } from './picker'
export { SecretValue } from './secret-value'
export { Callout } from './callout'
export { FormErrors, type FieldError } from './form'
export { type FieldControl } from './templates'
export { TimeChart, RangePicker, ChartFooter, type TimePoint, type TimeRange, type Marker, type Series, type SeriesStroke, type SeriesWeight, type Range } from './time-chart'
export { GitProviderLogo, type GitProviderType } from './git-provider-logo'
export { Drop } from './drop'
export { ShellSlotsProvider, useShellSlots, type ShellSlots } from './shell-slots'
export { Breakdown, GeoMap, Sparkline, StatusStrip, ScoreRing, CalendarHeatmap, Funnel, Flow, Waterfall, StackTrace, LogLines, Stages, Histogram, quantile, Live,
  type BreakdownRow, type GeoRow, type StatusBucket, type ActivityDay, type FunnelStep, type FlowRow, type Span, type Frame, type LogLine, type Stage, type HistBucket, type Pct } from './viz'
export { ProjectMark } from './project-mark'
