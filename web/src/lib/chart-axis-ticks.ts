// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Y-axis tick generation shared by every value chart.
 *
 * Recharts picks "nice" ticks for a numeric domain without knowing how they
 * will be printed, so a small range such as 0–1.3 % gets ticks 0, 0.35, 0.7,
 * 1.05, 1.4 which a whole-number formatter renders as "0%, 0%, 1%, 1%, 1%".
 * These helpers choose the ticks together with their formatting: the step is
 * a 1/2/2.5/5 × 10ⁿ value, the decimals shown follow the step, and when a
 * caller's formatter still collapses neighbouring ticks into the same label,
 * the step widens until every label is distinct.
 */

/** Tick-time context passed to a chart's tick formatter. */
export type AxisTickContext = {
  /** Distance between neighbouring ticks. */
  step: number
  /** Decimal places needed to tell neighbouring ticks apart. */
  decimals: number
}

export type AxisTickFormatter = (value: number, ctx: AxisTickContext) => string

export type YAxisTicks = AxisTickContext & {
  /** Tick values, ascending, evenly spaced by `step`. */
  ticks: number[]
  /** Axis domain: the first and last tick. */
  domain: [number, number]
}

export type YAxisTickOptions = {
  /** Upper bound on the number of ticks. Defaults to 6. */
  maxTicks?: number
  /** Only whole-number ticks (counts, connections). */
  integer?: boolean
  /** Extend the domain to include zero. Defaults to true. */
  includeZero?: boolean
  /**
   * The labels the chart will print. When it maps two ticks to the same
   * text the step is widened, so the axis never repeats a label.
   */
  format?: AxisTickFormatter
}

const NICE_MANTISSAS = [1, 2, 2.5, 5] as const
/** Bound on step widening; each iteration is one rung of the 1/2/2.5/5 ladder. */
const MAX_STEP_CANDIDATES = 48

/** Decimal places needed to print every multiple of `step` exactly. */
export function stepDecimals(step: number): number {
  if (!Number.isFinite(step) || step <= 0) return 0
  for (let decimals = 0; decimals <= 12; decimals++) {
    const scaled = step * 10 ** decimals
    if (Math.abs(scaled - Math.round(scaled)) < 1e-9 * Math.max(1, scaled)) {
      return decimals
    }
  }
  return 12
}

/** `value` with exactly `decimals` places, without a negative zero. */
export function formatAxisNumber(value: number, decimals: number): string {
  if (!Number.isFinite(value)) return ''
  const fixed = value.toFixed(Math.max(0, Math.min(20, decimals)))
  return Number(fixed) === 0 ? fixed.replace(/^-/, '') : fixed
}

/** Default label: the number at the precision its step needs. */
export const defaultAxisTickFormat: AxisTickFormatter = (value, ctx) =>
  formatAxisNumber(value, ctx.decimals)

/** The ladder of nice steps, starting at the first one ≥ `rawStep`. */
function* niceSteps(rawStep: number, integer: boolean): Generator<number> {
  let exponent = Math.floor(Math.log10(rawStep))
  for (;;) {
    const magnitude = 10 ** exponent
    for (const mantissa of NICE_MANTISSAS) {
      // Round away float noise (e.g. 0.30000000000000004).
      const step = Number((mantissa * magnitude).toPrecision(12))
      if (step < rawStep * (1 - 1e-9)) continue
      if (integer && (step < 1 || !Number.isInteger(step))) continue
      yield step
    }
    exponent++
  }
}

function ticksForStep(lo: number, hi: number, step: number): number[] {
  const decimals = stepDecimals(step)
  const first = Math.floor(lo / step + 1e-9)
  const last = Math.ceil(hi / step - 1e-9)
  const ticks: number[] = []
  for (let i = first; i <= last; i++) {
    ticks.push(Number((i * step).toFixed(decimals)))
  }
  return ticks
}

function labelsAreDistinct(
  ticks: number[],
  ctx: AxisTickContext,
  format: AxisTickFormatter
): boolean {
  const labels = new Set<string>()
  for (const tick of ticks) {
    const label = format(tick, ctx)
    if (labels.has(label)) return false
    labels.add(label)
  }
  return true
}

/**
 * Evenly spaced Y-axis ticks covering `[min, max]` whose printed labels are
 * all different. Non-finite bounds and a zero-height range (a flat series)
 * still produce a usable axis.
 */
export function buildYAxisTicks(
  min: number,
  max: number,
  options: YAxisTickOptions = {}
): YAxisTicks {
  const maxTicks = Math.max(2, Math.floor(options.maxTicks ?? 6))
  const integer = options.integer ?? false
  const includeZero = options.includeZero ?? true
  const format = options.format ?? defaultAxisTickFormat

  let lo = Number.isFinite(min) ? min : 0
  let hi = Number.isFinite(max) ? max : 0
  if (lo > hi) [lo, hi] = [hi, lo]
  if (includeZero) {
    lo = Math.min(0, lo)
    hi = Math.max(0, hi)
  }
  if (hi === lo) {
    // A flat series: give it one unit (or one unit of its own magnitude) of
    // headroom so the line sits inside the plot instead of on its edge.
    const pad = lo === 0 ? 1 : 10 ** Math.floor(Math.log10(Math.abs(lo)))
    if (lo >= 0 && includeZero) hi = lo + pad
    else {
      lo -= pad
      hi += pad
    }
  }

  // The smallest step that fits within maxTicks intervals.
  const rawStep = (hi - lo) / (maxTicks - 1)
  let candidates = 0
  for (const step of niceSteps(rawStep, integer)) {
    if (++candidates > MAX_STEP_CANDIDATES) break
    const ticks = ticksForStep(lo, hi, step)
    if (ticks.length > maxTicks) continue
    const ctx = { step, decimals: stepDecimals(step) }
    if (!labelsAreDistinct(ticks, ctx, format)) continue
    return { ...ctx, ticks, domain: [ticks[0], ticks[ticks.length - 1]] }
  }

  // Unreachable for any formatter that eventually separates values far
  // enough apart; keep a two-tick axis rather than throwing.
  const step = hi - lo
  return {
    step,
    decimals: stepDecimals(step),
    ticks: [lo, hi],
    domain: [lo, hi],
  }
}

/** Finite numbers among `values`, for computing an axis range. */
export function finiteValues(values: Iterable<unknown>): number[] {
  const out: number[] = []
  for (const value of values) {
    if (typeof value === 'number' && Number.isFinite(value)) out.push(value)
  }
  return out
}

/**
 * `<YAxis>` props for a plain recharts chart: explicit ticks, the matching
 * domain, and a formatter that prints each tick with the precision its
 * step needs. `format` receives the same context, so unit-aware formatters
 * (`%`, `ms`) can still keep their unit while honouring the precision.
 */
export function yAxisTickProps(
  values: Iterable<unknown>,
  options: YAxisTickOptions = {}
): {
  ticks: number[]
  domain: [number, number]
  tickFormatter: (value: number) => string
  interval: 0
} {
  let min = Infinity
  let max = -Infinity
  for (const value of finiteValues(values)) {
    if (value < min) min = value
    if (value > max) max = value
  }
  const axis = buildYAxisTicks(
    Number.isFinite(min) ? min : 0,
    Number.isFinite(max) ? max : 0,
    options
  )
  const format = options.format ?? defaultAxisTickFormat
  const ctx = { step: axis.step, decimals: axis.decimals }
  return {
    ticks: axis.ticks,
    domain: axis.domain,
    tickFormatter: (value: number) => format(value, ctx),
    // Draw every tick we chose; recharts would otherwise drop some to save room.
    interval: 0,
  }
}
