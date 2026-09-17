// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ReactNode } from 'react'

/** A `date` / `time` / `datetime-local` control, by what it enters. */
export type TemporalKind = 'datetime-local' | 'date' | 'time'

/** Seconds are shown only where the operation is second-precise (point-in-time restore). */
export type Precision = 'minute' | 'second'

/** An anchor that fills the absolute field: `now`, `−1h`, `last backup`. A thunk so "now" is read when it is pressed, not when the page rendered. */
export type Preset = { label: string; value: string | (() => string) }

/** A quick range, in hours back from `to`. `days` gates it against retention exactly as a chart's `Range` does. */
export type Quick = { label: string; hours: number }

/** The explicit "no expiry" option. An empty date never means forever. */
export type NeverOption = {
  label: string
  on: boolean
  onChange: (on: boolean) => void
}

export const PAD = (n: number) => String(n).padStart(2, '0')

/** A `Date` as the ISO local stamp the inputs here read and write. Wall clock, never converted. */
export function toStamp(
  d: Date,
  o: { kind?: TemporalKind; precision?: Precision } = {}
): string {
  const date = `${d.getFullYear()}-${PAD(d.getMonth() + 1)}-${PAD(d.getDate())}`
  const time = `${PAD(d.getHours())}:${PAD(d.getMinutes())}${o.precision === 'second' ? `:${PAD(d.getSeconds())}` : ''}`
  if (o.kind === 'date') return date
  if (o.kind === 'time') return time
  return `${date}T${time}`
}

/* ── the shared strip ───────────────────────────────────────────────── */

export type StripItem = {
  label: ReactNode
  pressed?: boolean
  /** Past the plan's retention: struck through with the reason in `title`, never hidden. */
  gated?: boolean
  title?: string
  onClick: () => void
}

/* ── the temporal control ───────────────────────────────────────────── */

export type ControlProps = {
  kind: TemporalKind
  value: string
  onChange: (v: string) => void
  /** The zone the value is read in, named: `UTC`, `Europe/Madrid`. Never guessed. */
  zone: string
  /** Given, the zone becomes a Picker in the same Field instead of a fact. */
  onZoneChange?: (z: string) => void
  zones?: string[]
  precision?: Precision
  min?: string
  max?: string
  presets?: Preset[]
  never?: NeverOption
  /** Adds the distance from `now` as a fact ("in 91 days"). */
  now?: Date | number
  disabled?: boolean
  id?: string
  'aria-describedby'?: string
  'aria-invalid'?: boolean
  /** Names one half of a range, where the Field's label belongs to the pair and "from" and "to" are only visible text. */
  'aria-label'?: string
  onTouch?: () => void
  width?: string
  /** Off on the "from" half of a range, where the pair shares one zone and saying it twice is the same fact twice. */
  showZone?: boolean
}

/* ── the fields ─────────────────────────────────────────────────────── */

export type DateTimeFieldProps = Omit<
  ControlProps,
  'kind' | 'aria-describedby' | 'aria-invalid' | 'onTouch'
> & {
  label: string
  hint?: ReactNode
  /** The caller's fault. When absent the field shows its own bound fault, on blur. */
  error?: string
  optional?: boolean
}

/* ── duration ───────────────────────────────────────────────────────── */

export type DurationUnit = 's' | 'min' | 'h' | 'd'

/* ── schedule ───────────────────────────────────────────────────────── */

/** 0 is Sunday, as `Date#getDay` counts. */
export type Weekday = 0 | 1 | 2 | 3 | 4 | 5 | 6

/** The next `count` runs of `HH:MM` on `days`, so a schedule is verifiable before it is saved. */
export function nextRuns(
  time: string,
  days: Weekday[] | undefined,
  from: Date,
  count = 3
): Date[] {
  const [h, m] = time.split(':').map(Number)
  if (!Number.isFinite(h) || !Number.isFinite(m)) return []
  const out: Date[] = []
  const cursor = new Date(
    from.getFullYear(),
    from.getMonth(),
    from.getDate(),
    h,
    m,
    0,
    0
  )
  for (let i = 0; i < 400 && out.length < count; i += 1) {
    if (
      cursor > from &&
      (!days || days.length === 0 || days.includes(cursor.getDay() as Weekday))
    )
      out.push(new Date(cursor))
    cursor.setDate(cursor.getDate() + 1)
  }
  return out
}
