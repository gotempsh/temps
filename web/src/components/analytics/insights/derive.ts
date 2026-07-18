import type { EventTimeline, PagePathInfo } from '@/api/client/types.gen'
import type { Insight } from './types'

/**
 * Pure stat-insight derivers. Every function here takes data the page has
 * already fetched and returns zero or more `Insight`s — no queries, no
 * side effects. Each deriver bails out below a minimum sample size so we
 * never present noise as a finding.
 */

/** Below this many total units a breakdown is too thin to editorialize. */
const MIN_BREAKDOWN_TOTAL = 20
/** Below this many total points a timeline is too thin for trend/peak claims. */
const MIN_TIMING_TOTAL = 50

export function formatShare(pct: number): string {
  if (!Number.isFinite(pct) || pct <= 0) return '0%'
  return pct >= 10 ? `${Math.round(pct)}%` : `${pct.toFixed(1)}%`
}

export function formatSeconds(seconds: number): string {
  const s = Math.round(seconds)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  const rest = s % 60
  return rest ? `${m}m ${rest}s` : `${m}m`
}

/**
 * How a breakdown dimension should be narrated: acquisition dimensions
 * (channels, referrers, UTM) warn about over-concentration, tech and geo
 * dimensions just describe the distribution.
 */
export type BreakdownFlavor =
  'acquisition' | 'tech' | 'geo' | 'content' | 'generic'

export interface BreakdownRow {
  value: string
  count: number
}

export interface BreakdownInsightInput {
  rows: BreakdownRow[]
  singular: string
  plural: string
  flavor: BreakdownFlavor
  /** What the counts measure, e.g. "visitors". */
  unitLabel?: string
}

export function deriveBreakdownInsights({
  rows,
  singular,
  plural,
  flavor,
  unitLabel = 'visitors',
}: BreakdownInsightInput): Insight[] {
  const sorted = rows
    .filter((r) => r.count > 0)
    .slice()
    .sort((a, b) => b.count - a.count)
  const total = sorted.reduce((sum, r) => sum + r.count, 0)
  if (total < MIN_BREAKDOWN_TOTAL || sorted.length === 0) return []

  const insights: Insight[] = []
  const [top, second] = sorted
  const topShare = (top.count / total) * 100

  insights.push({
    id: `stat-top-${plural}`,
    source: 'stat',
    tone: 'neutral',
    title: `${top.value} is your top ${singular}.`,
    detail: second
      ? `${formatShare(topShare)} of ${unitLabel}, ahead of ${second.value} at ${formatShare((second.count / total) * 100)}.`
      : `All ${total.toLocaleString()} ${unitLabel} in this period came from it.`,
    value: formatShare(topShare),
  })

  if (flavor === 'acquisition' && topShare >= 70 && sorted.length > 1) {
    insights.push({
      id: `stat-concentration-${plural}`,
      source: 'stat',
      tone: 'negative',
      title: `Traffic depends heavily on a single ${singular}.`,
      detail: `${top.value} drives ${formatShare(topShare)} of ${unitLabel} — a dip there would hit hard.`,
    })
  }

  const unattributed = sorted.find(
    (r) =>
      r.value === 'Unknown' ||
      r.value === '' ||
      (flavor === 'acquisition' && r.value === 'Direct')
  )
  if (unattributed && (unattributed.count / total) * 100 >= 40) {
    const share = formatShare((unattributed.count / total) * 100)
    insights.push({
      id: `stat-unattributed-${plural}`,
      source: 'stat',
      tone: 'neutral',
      title:
        unattributed.value === 'Direct'
          ? 'A large share of traffic arrives direct.'
          : `A large share of ${plural} is unattributed.`,
      detail:
        unattributed.value === 'Direct'
          ? `${share} of ${unitLabel} have no referrer — bookmarks, apps, or untagged links.`
          : `${share} of ${unitLabel} could not be attributed to a known ${singular}.`,
      value: share,
    })
  }

  if (sorted.length >= 6 && topShare < 35) {
    const top3Share =
      (sorted.slice(0, 3).reduce((sum, r) => sum + r.count, 0) / total) * 100
    insights.push({
      id: `stat-spread-${plural}`,
      source: 'stat',
      tone: 'neutral',
      title: `No single ${singular} dominates.`,
      detail: `Your top 3 ${plural} combined account for ${formatShare(top3Share)} of ${unitLabel}.`,
    })
  }

  return insights
}

const WEEKDAYS = [
  'Sunday',
  'Monday',
  'Tuesday',
  'Wednesday',
  'Thursday',
  'Friday',
  'Saturday',
]

function hourLabel(hour: number): string {
  const h = hour % 12 === 0 ? 12 : hour % 12
  return `${h} ${hour < 12 ? 'AM' : 'PM'}`
}

/**
 * Trend (first half vs second half of the range) and peak-time insights
 * from an hourly timeline. Times are interpreted in the viewer's local
 * timezone, matching how the dashboard charts render.
 */
export function deriveTimingInsights(
  points: EventTimeline[],
  unitLabel = 'visitors'
): Insight[] {
  const parsed = points
    .map((p) => ({ time: new Date(p.date).getTime(), count: p.count }))
    .filter((p) => Number.isFinite(p.time))
    .sort((a, b) => a.time - b.time)
  const total = parsed.reduce((sum, p) => sum + p.count, 0)
  if (parsed.length < 2 || total < MIN_TIMING_TOTAL) return []

  const insights: Insight[] = []
  const spanMs = parsed[parsed.length - 1].time - parsed[0].time
  const spanDays = spanMs / 86_400_000

  // Trend: compare the two halves of the range.
  const midpoint = parsed[0].time + spanMs / 2
  let firstHalf = 0
  let secondHalf = 0
  for (const p of parsed) {
    if (p.time < midpoint) firstHalf += p.count
    else secondHalf += p.count
  }
  if (firstHalf >= 20) {
    const change = ((secondHalf - firstHalf) / firstHalf) * 100
    if (Math.abs(change) >= 20) {
      const rounded = Math.round(Math.abs(change))
      insights.push({
        id: 'stat-trend',
        source: 'stat',
        tone: change > 0 ? 'positive' : 'negative',
        title:
          change > 0 ? 'Traffic is trending up.' : 'Traffic is trending down.',
        detail: `${unitLabel[0].toUpperCase()}${unitLabel.slice(1)} in the second half of this range are ${rounded}% ${change > 0 ? 'higher' : 'lower'} than in the first half.`,
        value: `${change > 0 ? '+' : '-'}${rounded}%`,
      })
    }
  }

  // Peak hour, and peak weekday once the range covers a full week.
  const hourSums = new Array<number>(24).fill(0)
  const daySums = new Array<number>(7).fill(0)
  for (const p of parsed) {
    const d = new Date(p.time)
    hourSums[d.getHours()] += p.count
    daySums[d.getDay()] += p.count
  }
  const peakHour = hourSums.indexOf(Math.max(...hourSums))

  if (spanDays >= 6.5) {
    const peakDay = daySums.indexOf(Math.max(...daySums))
    const quietDay = daySums.indexOf(Math.min(...daySums.filter((c) => c >= 0)))
    insights.push({
      id: 'stat-peak-time',
      source: 'stat',
      tone: 'neutral',
      title: `${WEEKDAYS[peakDay]}s are your busiest day.`,
      detail: `Activity peaks around ${hourLabel(peakHour)}${quietDay !== peakDay ? `; ${WEEKDAYS[quietDay]}s are quietest` : ''}.`,
      value: hourLabel(peakHour),
    })
  } else if (spanDays >= 1) {
    insights.push({
      id: 'stat-peak-time',
      source: 'stat',
      tone: 'neutral',
      title: `Traffic peaks around ${hourLabel(peakHour)}.`,
      value: hourLabel(peakHour),
    })
  }

  return insights
}

export function derivePagesInsights(pages: PagePathInfo[]): Insight[] {
  const totalViews = pages.reduce((sum, p) => sum + p.page_view_count, 0)
  if (totalViews < MIN_BREAKDOWN_TOTAL || pages.length === 0) return []

  const insights: Insight[] = []
  const byViews = pages
    .slice()
    .sort((a, b) => b.page_view_count - a.page_view_count)
  const top = byViews[0]
  const topShare = (top.page_view_count / totalViews) * 100

  if (topShare >= 40 && pages.length > 1) {
    insights.push({
      id: 'stat-top-page',
      source: 'stat',
      tone: 'neutral',
      title: `${top.page_path} drives most of your views.`,
      detail: `${formatShare(topShare)} of ${totalViews.toLocaleString()} page views in this period.`,
      value: formatShare(topShare),
    })
  }

  // Busy page that visitors abandon quickly — a concrete candidate to improve.
  const quickExit = pages
    .filter(
      (p) =>
        p.session_count >= 20 &&
        p.avg_time_seconds != null &&
        p.avg_time_seconds < 15
    )
    .sort((a, b) => b.session_count - a.session_count)[0]
  if (quickExit && quickExit.avg_time_seconds != null) {
    insights.push({
      id: 'stat-quick-exit',
      source: 'stat',
      tone: 'negative',
      title: `${quickExit.page_path} loses visitors quickly.`,
      detail: `Visitors spend ${formatSeconds(quickExit.avg_time_seconds)} on average across ${quickExit.session_count.toLocaleString()} sessions.`,
      value: formatSeconds(quickExit.avg_time_seconds),
    })
  }

  const sticky = pages
    .filter(
      (p) =>
        p.session_count >= 10 &&
        p.avg_time_seconds != null &&
        p.avg_time_seconds >= 60 &&
        p.page_path !== quickExit?.page_path
    )
    .sort((a, b) => (b.avg_time_seconds ?? 0) - (a.avg_time_seconds ?? 0))[0]
  if (sticky && sticky.avg_time_seconds != null) {
    insights.push({
      id: 'stat-sticky-page',
      source: 'stat',
      tone: 'positive',
      title: `${sticky.page_path} holds attention longest.`,
      detail: `${formatSeconds(sticky.avg_time_seconds)} average time across ${sticky.session_count.toLocaleString()} sessions.`,
      value: formatSeconds(sticky.avg_time_seconds),
    })
  }

  return insights
}

/** Sessions-per-visitor signal for the overview page. */
export function deriveReturnRateInsight(
  visitors: number,
  sessions: number
): Insight | null {
  if (visitors < 30 || sessions <= 0) return null
  const ratio = sessions / visitors
  if (ratio >= 1.5) {
    return {
      id: 'stat-return-rate',
      source: 'stat',
      tone: 'positive',
      title: 'Visitors come back for more.',
      detail: `${ratio.toFixed(1)} sessions per visitor in this period.`,
      value: `${ratio.toFixed(1)}x`,
    }
  }
  if (ratio <= 1.05) {
    return {
      id: 'stat-return-rate',
      source: 'stat',
      tone: 'neutral',
      title: 'Most visits are one-and-done.',
      detail: `${sessions.toLocaleString()} sessions from ${visitors.toLocaleString()} visitors — almost nobody returns within this range.`,
    }
  }
  return null
}
