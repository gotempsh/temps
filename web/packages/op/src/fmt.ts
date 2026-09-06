// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The number, date and duration rules of `design-system/docs/content.md`,
 * written once so no screen has to remember them. Pure functions, no React,
 * no state: a component formats at render time and nothing else.
 *
 * The rules these enforce, in short (content.md §Numbers has the reasoning):
 *  - Thousands separators come from the operator's locale, never from a
 *    hand-rolled regex.
 *  - Bytes are decimal (kB, MB, GB): the operator compares them against an
 *    invoice and a provider dashboard, and those are decimal too.
 *  - Percentages carry one decimal by default; a rate that moves in
 *    hundredths of a point is the whole point of the number.
 *  - Time is relative under 24 hours and absolute after, always beside the
 *    id of the thing that happened (a deploy tag, a run id).
 *  - Nothing is an en dash. Zero is `0`. They are different facts.
 *
 * Rendering rules live with the components: mono, tabular, unit after the
 * value in muted ink. `Num` puts these strings on the page; these functions
 * only decide what the string says.
 */

/** Nothing to show. Zero is `0` and is not this. See content.md §Numbers. */
export const EMPTY = '–'

/** A BCP 47 tag, a list of them, or nothing for the runtime's own locale. */
export type Locale = string | string[] | undefined

const nothing = (n: unknown): n is null | undefined => n === null || n === undefined || (typeof n === 'number' && !Number.isFinite(n))

/**
 * A count or a measure, grouped for the operator's locale.
 * `digits` fixes the fraction digits exactly (`digits: 1` → `9.4`); left off,
 * the number keeps its own precision up to three places, as `Intl` does.
 *
 * ```ts
 * fmtNum(30800)            // "30,800"   (en) · "30.800" (de)
 * fmtNum(0.6135, { digits: 2 }) // "0.61"
 * fmtNum(null)             // "–"
 * ```
 */
export function fmtNum(n: number | null | undefined, o: { locale?: Locale; digits?: number } = {}): string {
  if (nothing(n)) return EMPTY
  return new Intl.NumberFormat(o.locale, o.digits === undefined ? undefined : { minimumFractionDigits: o.digits, maximumFractionDigits: o.digits }).format(n)
}

/**
 * A percentage. `basis: 'percent'` (the default) takes a number already on
 * the 0–100 scale; `basis: 'ratio'` takes 0–1 and scales it. One decimal by
 * default — `0.6%` and `0.61%` are different operational facts.
 *
 * ```ts
 * fmtPct(0.61)                             // "0.6%"
 * fmtPct(0.61, { digits: 2 })              // "0.61%"
 * fmtPct(31 / 4820, { basis: 'ratio' })    // "0.6%"
 * ```
 */
export function fmtPct(n: number | null | undefined, o: { locale?: Locale; digits?: number; basis?: 'percent' | 'ratio' } = {}): string {
  if (nothing(n)) return EMPTY
  const digits = o.digits ?? 1
  const value = o.basis === 'ratio' ? n * 100 : n
  return `${fmtNum(value, { locale: o.locale, digits })}%`
}

const DECIMAL = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'] as const
const BINARY = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const

/**
 * A size in bytes. Decimal by default (1 kB = 1000 B) because the number is
 * read next to a bandwidth bill and a provider's console, which are decimal;
 * pass `binary: true` for memory and page cache, where the machine's own
 * units are the honest ones, and the unit says so (`MiB`).
 * Under 1 kB the value stays whole; above it, one decimal until it is large
 * enough not to need one.
 *
 * ```ts
 * fmtBytes(212_000_000)                 // "212 MB"
 * fmtBytes(1536, { binary: true })      // "1.5 KiB"
 * fmtBytes(0)                           // "0 B"
 * ```
 */
export function fmtBytes(n: number | null | undefined, o: { locale?: Locale; binary?: boolean; digits?: number } = {}): string {
  if (nothing(n)) return EMPTY
  const step = o.binary ? 1024 : 1000
  const units = o.binary ? BINARY : DECIMAL
  const sign = n < 0 ? '-' : ''
  let v = Math.abs(n)
  let i = 0
  while (v >= step && i < units.length - 1) { v /= step; i += 1 }
  const digits = o.digits ?? (i === 0 ? 0 : v < 10 ? 1 : 0)
  return `${sign}${fmtNum(v, { locale: o.locale, digits })} ${units[i]}`
}

/**
 * How long something took, at the precision an operator acts on: two units,
 * never more (`41m 12s`, `2h 05m`, `3d 4h`). Under a minute it is one number
 * with its unit; a build that took `2m 25s` never reads `145s`.
 *
 * ```ts
 * fmtDuration(812)        // "812ms"
 * fmtDuration(9_400)      // "9.4s"
 * fmtDuration(2_472_000)  // "41m 12s"
 * ```
 */
export function fmtDuration(ms: number | null | undefined, o: { locale?: Locale } = {}): string {
  if (nothing(ms)) return EMPTY
  const sign = ms < 0 ? '-' : ''
  const abs = Math.abs(ms)
  const n = (v: number, digits = 0) => fmtNum(v, { locale: o.locale, digits })
  if (abs < 1) return `${sign}${n(abs * 1000)}µs`
  if (abs < 1000) return `${sign}${n(Math.round(abs))}ms`
  const s = abs / 1000
  if (s < 10) return `${sign}${n(s, 1)}s`
  if (s < 60) return `${sign}${n(Math.round(s))}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${sign}${n(m)}m ${String(Math.round(s % 60)).padStart(2, '0')}s`
  const h = Math.floor(m / 60)
  if (h < 24) return `${sign}${n(h)}h ${String(m % 60).padStart(2, '0')}m`
  return `${sign}${n(Math.floor(h / 24))}d ${h % 24}h`
}

const DATE = (d: Date | string | number): Date => (d instanceof Date ? d : new Date(d))

/**
 * A wall-clock time. Rendered in the reader's own zone by default, because
 * that is the clock they are looking at while the incident happens. Pass
 * `tz` for anything that will be quoted or compared across people
 * (`tz: 'UTC'` in an exported report) — a named zone always prints its name,
 * so a pasted timestamp can never be read in the wrong one.
 *
 * ```ts
 * fmtAbsolute('2026-09-06T20:33:00Z')                  // "Sep 6 at 20:33"
 * fmtAbsolute('2026-09-06T20:33:00Z', { tz: 'UTC' })   // "Sep 6 at 20:33 UTC"
 * ```
 */
export function fmtAbsolute(date: Date | string | number | null | undefined, o: { locale?: Locale; tz?: string; seconds?: boolean; year?: boolean } = {}): string {
  if (date === null || date === undefined || date === '') return EMPTY
  const d = DATE(date)
  if (Number.isNaN(d.getTime())) return typeof date === 'string' ? date : EMPTY
  return new Intl.DateTimeFormat(o.locale ?? 'en', {
    month: 'short',
    day: 'numeric',
    ...(o.year ? { year: 'numeric' as const } : null),
    hour: '2-digit',
    minute: '2-digit',
    ...(o.seconds ? { second: '2-digit' as const } : null),
    hour12: false,
    ...(o.tz ? { timeZone: o.tz, timeZoneName: 'short' as const } : null),
  }).format(d)
}

const MINUTE = 60_000
const HOUR = 3_600_000
const DAY = 86_400_000

/**
 * When something happened. Relative under 24 hours (`41m ago`), absolute
 * after — a reader cannot subtract "9 days ago" from today, and "just now"
 * is not a time. Always render it next to the id of the thing that happened
 * (`dep_91a`), and give the element a `title` of {@link fmtAbsolute} so the
 * exact stamp is one hover away.
 *
 * ```ts
 * fmtRelative(t, now)  // "41m ago" · "10h ago" · "Sep 6 at 20:33"
 * ```
 */
export function fmtRelative(date: Date | string | number | null | undefined, now: Date | number = Date.now(), o: { locale?: Locale; tz?: string } = {}): string {
  if (date === null || date === undefined || date === '') return EMPTY
  const d = DATE(date)
  if (Number.isNaN(d.getTime())) return typeof date === 'string' ? date : EMPTY
  const delta = d.getTime() - (now instanceof Date ? now.getTime() : now)
  const abs = Math.abs(delta)
  if (abs >= DAY) return fmtAbsolute(d, o)
  const ago = delta <= 0
  const body = abs < MINUTE ? `${Math.max(0, Math.round(abs / 1000))}s` : abs < HOUR ? `${Math.round(abs / MINUTE)}m` : `${Math.round(abs / HOUR)}h`
  return ago ? `${body} ago` : `in ${body}`
}

/**
 * A count and its noun, with the plural the operator's locale actually uses.
 * Never build one by concatenating `n + ' ' + word + 's'`: that is a sentence
 * assembled from fragments, and it does not survive translation
 * (localisation.md §Plurals).
 *
 * ```ts
 * fmtCount(1, 'deploy', 'deploys')  // "1 deploy"
 * fmtCount(6, 'deploy', 'deploys')  // "6 deploys"
 * fmtCount(0, 'issue', 'issues')    // "0 issues"
 * ```
 */
export function fmtCount(n: number | null | undefined, singular: string, plural = `${singular}s`, o: { locale?: Locale } = {}): string {
  if (nothing(n)) return `${EMPTY} ${plural}`
  const rule = new Intl.PluralRules(o.locale).select(n)
  return `${fmtNum(n, { locale: o.locale })} ${rule === 'one' ? singular : plural}`
}
