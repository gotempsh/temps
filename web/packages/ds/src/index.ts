// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Operator component library ("op"). These are the components the three
 * page templates are built from, and the ones a new console screen should
 * reach for first. See docs/design-system-handoff.md §6.
 */
export { Kbd, KbdPair, MOD, IS_MAC } from './kbd'
export { Status, StatusLine, AttentionHost, Phrase, worst, GLYPH, GLYPH_CLASS, glyphClass, STATE_RANK, type State, type StatusItem } from './status'
export { Num, Metric, MetricGrid } from './num'
export { fmtNum, fmtPct, fmtBytes, fmtDuration, fmtRelative, fmtAbsolute, fmtStamp, fmtCount, EMPTY, type Locale } from './fmt'
export { PageState, type PageStateProps } from './page-state'
export { EchoDialog } from './echo-dialog'
export { Ledger, Detail, Settings, Field, Segmented, PageTitle, Pager, ActionBar, SectionTitle, Section,
  Columns, Lede, KeyValue, Timeline, type KV, type TimelineItem, PAGE_SIZES, type Page, type Crumb, type LedgerRow, type LedgerColumn, type LedgerSort } from './templates'
export { Picker, type PickerOption } from './picker'
export { SecretValue } from './secret-value'
export { Callout } from './callout'
export { FormErrors, type FieldError } from './form'
export { type FieldControl } from './templates'
export { TimeChart, RangePicker, ChartFooter, type TimePoint, type TimeRange, type Marker, type Series, type SeriesStroke, type SeriesWeight, type Range, type Band, type Anomaly, type Compare, outside, vsExpected } from './time-chart'
export { GitProviderLogo, type GitProviderType } from './git-provider-logo'
export { Drop } from './drop'
export { ShellSlotsProvider, useShellSlots, type ShellSlots } from './shell-slots'
export { Breakdown, GeoMap, Sparkline, StatusStrip, ScoreRing, CalendarHeatmap, Funnel, Flow, Waterfall, StackTrace, LogLines, Stages, Histogram, quantile, Live,
  type BreakdownRow, type GeoRow, type StatusBucket, type ActivityDay, type FunnelStep, type FlowRow, type Span, type Frame, type LogLine, type Stage, type HistBucket, type Pct } from './viz'
export { DateTimeField, DateField, TimeField, DateTimeRangeField, DurationField, ScheduleField, Strip, nextRuns, toStamp,
  type TemporalKind, type Precision, type Preset, type Quick, type NeverOption, type StripItem, type DurationUnit, type Weekday, type DateTimeFieldProps } from './datetime'
export { ProjectMark } from './project-mark'
/* The second wave of visualisations: the forms the Temps console needs that
   `viz.tsx` did not have. See design-system/docs/data-viz.md §§9–21. */
export { Figure, DataTable, InkPatterns, StateWord, useReadout, ReadoutLive, inkCell, inkStep,
  INK_FILL, INK_FILL_OPACITY, INK_FILL_WORD, INK_LAYER_ORDER, INK_STEPS, INK_TONE, type InkLayerFill } from './viz-ink'
export { BandChart, StackedInk, LatencyHeatmap, StateTimeline, WindowTimeline, SessionTimeline,
  type InkLayer, type LatencyRow, type StateSegment, type WindowMark, type SessionEvent, type Worse, type Excursion } from './viz-time'
export { PercentileLadder, CohortGrid, DeltaTable, type Rung, type Cohort, type DeltaRow } from './viz-grid'
export { PathTree, Topology, type PathNode, type TopoNode, type TopoLink } from './viz-graph'
export { UsageBar, Gauge } from './viz-usage'
export { ToolRow, Proposal, Provenance, StreamBlock, AgentQuestion, AgentSources, RunAside,
  AgentRow, AgentInset, AgentDiff, AgentGlyph, AgentKindIcon, TOOL_STATE, toolKind, toolIcon,
  type ToolState, type ToolKind, type ToolApproval, type StreamKind } from './agent'
/* Inspect a ledger row beside the list: a tool screen inspects in a panel,
   a record is a page. See design-system/docs/design-system-handoff.md §6. */
export { Inspector, type InspectorAnchor } from './inspector'
/* The fourth page template: a page that is read top to bottom (a post, a docs
   page, a changelog entry). See design-system/docs/content-pages.md. */
export { Article, CodeBlock, ImageFigure, type ArticleAuthor, type ArticleHeading } from './article'
/* A copy answers on the control it was pressed on. See notifications.md. */
export { CopyAction, useCopy, CopyIcon, COPY_HOLD_MS, type CopyState } from './copy'
/* The URL is the state: one query key as a piece of state, and the rules that
   keep an address rebuildable. See design-system/docs/design-system-handoff.md
   §6 "useUrlState" and docs/requirements.md. Needs `react-router`. */
export { useUrlState, useUrlNumber, useUrlPatch, useUrlWindow, useUrlSort, useUrlText, forNewView, VIEW_KEYS, KEPT_ON_NAVIGATION,
  type ViewKey, type UrlWindow, type UrlSort } from './url-state'
