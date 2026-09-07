// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type ReactNode } from 'react'
import { cn } from './lib/cn'
import { Field } from './templates'
import { Picker } from './picker'
import { fmtCount, fmtDuration, fmtStamp } from './fmt'

/* ────────────────────────────────────────────────────────────────────────
   Dates, times, ranges, durations and schedules as form controls
   (design-system/docs/forms.md §"Dates, times and ranges").

   One decision runs through all of them: typed entry first. Every control
   here is a text input the operator can type into — a native
   `date` / `time` / `datetime-local` under the ink skin, so the segments
   step with ↑/↓ and the browser's own picker is the accelerator. A calendar
   widget is never the only way in, because an operator reading a stamp out
   of a log pastes it; they do not hunt for it in a grid.

   The value every control speaks is an ISO local stamp with no zone
   (`2026-09-06T20:33`), and the zone is a fact rendered beside the control.
   Nothing here converts a wall clock into another zone: a control that
   guesses is a control that silently restores to the wrong second.
   ──────────────────────────────────────────────────────────────────────── */

const INPUT = 'h-8 border bg-background px-2 font-mono text-xs tabular-nums focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring disabled:cursor-not-allowed disabled:opacity-50'

/** A `date` / `time` / `datetime-local` control, by what it enters. */
export type TemporalKind = 'datetime-local' | 'date' | 'time'
/** Seconds are shown only where the operation is second-precise (point-in-time restore). */
export type Precision = 'minute' | 'second'
/** An anchor that fills the absolute field: `now`, `−1h`, `last backup`. A thunk so "now" is read when it is pressed, not when the page rendered. */
export type Preset = { label: string; value: string | (() => string) }
/** A quick range, in hours back from `to`. `days` gates it against retention exactly as a chart's `Range` does. */
export type Quick = { label: string; hours: number }
/** The explicit "no expiry" option. An empty date never means forever. */
export type NeverOption = { label: string; on: boolean; onChange: (on: boolean) => void }

const PAD = (n: number) => String(n).padStart(2, '0')

/** A `Date` as the ISO local stamp the inputs here read and write. Wall clock, never converted. */
export function toStamp(d: Date, o: { kind?: TemporalKind; precision?: Precision } = {}): string {
  const date = `${d.getFullYear()}-${PAD(d.getMonth() + 1)}-${PAD(d.getDate())}`
  const time = `${PAD(d.getHours())}:${PAD(d.getMinutes())}${o.precision === 'second' ? `:${PAD(d.getSeconds())}` : ''}`
  if (o.kind === 'date') return date
  if (o.kind === 'time') return time
  return `${date}T${time}`
}

/** Milliseconds for an ISO local stamp, for comparing one against a bound. `null` when it is not a stamp yet (a half-typed date is not an error). */
function ms(v: string | undefined): number | null {
  if (!v) return null
  const t = Date.parse(v.length <= 5 && v.includes(':') ? `1970-01-01T${v}` : v)
  return Number.isNaN(t) ? null : t
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

/**
 * One strip of anchors in one frame: a chart's quick ranges, a date field's
 * presets, a schedule's weekdays. Written once so a preset and a quick range
 * are the same control and the gating (struck through, still pressable, calls
 * `onGated`) cannot drift between them.
 */
export function Strip({ items, after, className, label }: { items: StripItem[]; after?: ReactNode; className?: string; label?: string }) {
  return (
    <div role={label ? 'group' : undefined} aria-label={label} className={cn('op-scroll-x flex max-w-full border text-[11px]', className)}>
      {items.map((it, i) => (
        <button
          key={i}
          type="button"
          aria-pressed={it.pressed}
          title={it.title}
          onClick={it.onClick}
          className={cn('h-7 shrink-0 px-2', i > 0 && 'border-l', it.pressed ? 'bg-foreground text-background' : it.gated ? 'text-muted-foreground line-through decoration-[var(--op-rule-soft)] hover:bg-muted' : 'hover:bg-muted')}
        >
          {it.label}
        </button>
      ))}
      {after}
    </div>
  )
}

/* ── the temporal control ───────────────────────────────────────────── */

type ControlProps = {
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

const DAY_MS = 86_400_000

function stampOf(v: string, kind: TemporalKind, precision: Precision) {
  return fmtStamp(v, { precision: kind === 'date' ? 'day' : precision })
}

/** The window a control accepts, as one sentence for the hint. Stated once, here. */
function windowHint(kind: TemporalKind, precision: Precision, zone: string, min?: string, max?: string): string | undefined {
  if (!min && !max) return undefined
  if (min && max) return `from ${stampOf(min, kind, precision)} to ${stampOf(max, kind, precision)} ${zone}`
  if (min) return `from ${stampOf(min, kind, precision)} ${zone} onward`
  return `up to ${stampOf(max!, kind, precision)} ${zone}`
}

/** The bound fault, as a state word and a sentence that names the edge and the fix. */
function boundFault(kind: TemporalKind, precision: Precision, zone: string, value: string, min?: string, max?: string): string | undefined {
  const v = ms(value)
  if (v === null) return undefined
  const lo = ms(min)
  const hi = ms(max)
  const noun = kind === 'time' ? 'time' : kind === 'date' ? 'date' : 'stamp'
  if (lo !== null && v < lo) return `out of window · ${stampOf(value, kind, precision)} is before ${stampOf(min!, kind, precision)} ${zone}; pick a later ${noun}`
  if (hi !== null && v > hi) return `out of window · ${stampOf(value, kind, precision)} is after ${stampOf(max!, kind, precision)} ${zone}; pick an earlier ${noun}`
  return undefined
}

function TemporalControl({ kind, value, onChange, zone, onZoneChange, zones, precision = 'minute', min, max, presets, never, now, disabled, id, onTouch, width, showZone = true, ...aria }: ControlProps) {
  const off = never?.on ?? false
  const distance = useMemo(() => {
    if (off || now === undefined || kind === 'time') return undefined
    const v = ms(value)
    if (v === null) return undefined
    const days = Math.round((v - (now instanceof Date ? now.getTime() : now)) / DAY_MS)
    if (days === 0) return 'today'
    return days > 0 ? `in ${fmtCount(days, 'day')}` : `${fmtCount(-days, 'day')} ago`
  }, [off, now, value, kind])

  const anchors: StripItem[] = [
    ...(presets ?? []).map((p) => {
      const v = typeof p.value === 'string' ? p.value : undefined
      return {
        label: p.label,
        pressed: !off && v !== undefined && v === value,
        onClick: () => { never?.onChange(false); onChange(typeof p.value === 'string' ? p.value : p.value()); onTouch?.() },
      }
    }),
    ...(never ? [{ label: never.label, pressed: off, onClick: () => never.onChange(!off) }] : []),
  ]

  return (
    <span className="block space-y-1.5">
      <span className="flex flex-wrap items-center gap-2">
        <input
          {...aria}
          id={id}
          type={kind}
          value={off ? '' : value}
          disabled={disabled || off}
          min={min}
          max={max}
          step={kind !== 'date' && precision === 'second' ? 1 : undefined}
          onChange={(e) => onChange(e.target.value)}
          onBlur={() => onTouch?.()}
          className={cn(INPUT, width ?? (kind === 'time' ? 'w-28' : kind === 'date' ? 'w-44' : precision === 'second' ? 'w-60' : 'w-52'))}
        />
        {!showZone ? null : onZoneChange ? (
          <Picker label="time zone" value={zone} onChange={onZoneChange} options={(zones ?? [zone]).map((z) => ({ value: z }))} className="h-8 w-auto min-w-40 text-xs" width="240px" />
        ) : (
          <span className="font-mono text-[11px] text-muted-foreground">{zone}</span>
        )}
      </span>
      {anchors.length > 0 && <Strip items={anchors} label="presets" className="w-max" />}
      {(off || distance) && <span className="block font-mono text-[11px] text-muted-foreground">{off ? never!.label : distance}</span>}
    </span>
  )
}

/* ── the fields ─────────────────────────────────────────────────────── */

export type DateTimeFieldProps = Omit<ControlProps, 'kind' | 'aria-describedby' | 'aria-invalid' | 'onTouch'> & {
  label: string
  hint?: ReactNode
  /** The caller's fault. When absent the field shows its own bound fault, on blur. */
  error?: string
  optional?: boolean
}

function TemporalField({ kind, label, hint, error, optional, ...rest }: DateTimeFieldProps & { kind: TemporalKind }) {
  const [touched, setTouched] = useState(false)
  const precision = rest.precision ?? 'minute'
  const bound = rest.never?.on ? undefined : boundFault(kind, precision, rest.zone, rest.value, rest.min, rest.max)
  // Blur validates; once a field is in error it re-checks every render, so the message clears as it is fixed.
  const shown = error ?? (touched ? bound : undefined)
  const bounds = windowHint(kind, precision, rest.zone, rest.min, rest.max)
  const advice = [hint, bounds].filter(Boolean)
  return (
    <Field
      label={label}
      optional={optional}
      error={shown}
      id={rest.id}
      hint={advice.length ? <>{advice.map((a, i) => <span key={i}>{i > 0 && ' · '}{a}</span>)}</> : undefined}
    >
      {(c) => <TemporalControl {...rest} {...c} kind={kind} onTouch={() => setTouched(true)} />}
    </Field>
  )
}

/**
 * A date and a time in one control, with its zone beside it. The value is an
 * ISO local stamp (`2026-09-06T20:33`); `precision: 'second'` adds seconds and
 * `step=1`, for the one operation that is second-precise. `min`/`max` state the
 * window in the hint and fault on blur; `presets` fill the field and the field
 * stays the truth about what they wrote.
 */
export function DateTimeField(p: DateTimeFieldProps) { return <TemporalField {...p} kind="datetime-local" /> }

/** A calendar day. `never` makes "no expiry" an option word rather than an empty date. */
export function DateField(p: DateTimeFieldProps) { return <TemporalField {...p} kind="date" /> }

/** A time of day, 24h, `HH:MM`, with its zone beside it. */
export function TimeField(p: DateTimeFieldProps) { return <TemporalField {...p} kind="time" /> }

/* ── range ──────────────────────────────────────────────────────────── */

export function DateTimeRangeField({ from, to, onChange, zone, onZoneChange, zones, precision = 'minute', min, max, quick, retentionDays, retentionLabel, onGated, now, label, hint, error, optional, id, disabled }: {
  from: string
  to: string
  onChange: (from: string, to: string) => void
  zone: string
  onZoneChange?: (z: string) => void
  zones?: string[]
  precision?: Precision
  min?: string
  max?: string
  /** Quick windows, back from `to` (or from `now`). `hours` also decides whether retention gates them. */
  quick?: Quick[]
  retentionDays?: number
  retentionLabel?: string
  onGated?: (q: Quick) => void
  now?: Date | number
  label: string
  hint?: ReactNode
  error?: string
  optional?: boolean
  id?: string
  disabled?: boolean
}) {
  const [touched, setTouched] = useState(false)
  const order = ms(from) !== null && ms(to) !== null && ms(to)! <= ms(from)! ? `empty window · "to" is ${stampOf(to, 'datetime-local', precision)} and "from" is ${stampOf(from, 'datetime-local', precision)}; move "to" later` : undefined
  const bound = boundFault('datetime-local', precision, zone, from, min, max) ?? boundFault('datetime-local', precision, zone, to, min, max)
  const shown = error ?? (touched ? (bound ?? order) : undefined)
  const bounds = windowHint('datetime-local', precision, zone, min, max)
  const advice = [hint, bounds, retentionLabel ? `${retentionLabel} retention` : undefined].filter(Boolean)

  const anchor = now === undefined ? Date.now() : now instanceof Date ? now.getTime() : now
  const items: StripItem[] = (quick ?? []).map((q) => {
    const gated = retentionDays !== undefined && q.hours / 24 > retentionDays
    const end = ms(to) ?? anchor
    const next = { from: toStamp(new Date(end - q.hours * 3_600_000), { precision }), to: toStamp(new Date(end), { precision }) }
    return {
      label: q.label,
      // Moments, not strings: `2026-09-05T20:33` and `2026-09-05T20:33:00` are the same second.
      pressed: !gated && ms(next.from) === ms(from) && ms(next.to) === ms(to),
      gated,
      title: gated ? `beyond ${retentionLabel ?? 'the plan'} retention` : undefined,
      onClick: () => { if (gated) { onGated?.(q); return } onChange(next.from, next.to); setTouched(true) },
    }
  })

  const col = (which: 'from' | 'to') => (
    <span className="block min-w-0 space-y-1">
      <span className="op-label block">{which}</span>
      <TemporalControl
        kind="datetime-local"
        value={which === 'from' ? from : to}
        onChange={(v) => onChange(which === 'from' ? v : from, which === 'to' ? v : to)}
        zone={zone}
        onZoneChange={which === 'to' ? onZoneChange : undefined}
        zones={zones}
        precision={precision}
        min={min}
        max={max}
        disabled={disabled}
        id={which === 'from' ? id : undefined}
        // The Field's label names the pair; each half still needs its own name, or "to" is an unlabelled input.
        aria-label={`${label} ${which}`}
        onTouch={which === 'to' ? () => setTouched(true) : undefined}
        showZone={which === 'to'}
        width="w-full"
      />
    </span>
  )

  return (
    <Field label={label} optional={optional} error={shown} id={id} hint={advice.length ? <>{advice.map((a, i) => <span key={i}>{i > 0 && ' · '}{a}</span>)}</> : undefined}>
      <span className="block space-y-2">
        {/* Two fields, one row; below sm they stack, because a 390px row of two datetime inputs is two clipped inputs. */}
        <span className="grid gap-2 sm:grid-cols-2">{col('from')}{col('to')}</span>
        {items.length > 0 && <Strip items={items} label="quick ranges" className="w-max" />}
      </span>
    </Field>
  )
}

/* ── duration ───────────────────────────────────────────────────────── */

export type DurationUnit = 's' | 'min' | 'h' | 'd'
const UNIT_MS: Record<DurationUnit, number> = { s: 1000, min: 60_000, h: 3_600_000, d: 86_400_000 }
const UNIT_NAME: Record<DurationUnit, string> = { s: 'seconds', min: 'minutes', h: 'hours', d: 'days' }
/** `fmtDuration` always writes two units; a bound is a round number, so `365d 0h` reads as a bug. Trim the zero tail here rather than in `fmtDuration`, where the second unit is the point. */
const len = (v: number) => fmtDuration(v).replace(/ 0+[a-z]+$/, '')

/** The largest offered unit the value is a whole number of, so `2592000000` reads `30 d` and not `720 h`. */
function bestUnit(value: number, units: DurationUnit[]): DurationUnit {
  const order = (['d', 'h', 'min', 's'] as DurationUnit[]).filter((u) => units.includes(u))
  return order.find((u) => value > 0 && value % UNIT_MS[u] === 0) ?? order[order.length - 1] ?? 's'
}

/**
 * A duration the operator types: a number and a unit Picker, never a free-text
 * `30d` that has to be parsed and can be typed four ways. The value is
 * milliseconds; when the typed number and unit do not already read as the
 * duration (`90` in `min`), `fmtDuration` reads it back underneath in the same
 * words the rest of the console uses (`1h 30m`).
 */
export function DurationField({ value, onChange, units = ['s', 'min', 'h', 'd'], min, max, label, hint, error, optional, id, disabled }: {
  value: number
  onChange: (ms: number) => void
  units?: DurationUnit[]
  /** Bounds in milliseconds. Stated in the hint, faulted on blur. */
  min?: number
  max?: number
  label: string
  hint?: ReactNode
  error?: string
  optional?: boolean
  id?: string
  disabled?: boolean
}) {
  const [unit, setUnit] = useState<DurationUnit>(() => bestUnit(value, units))
  const [touched, setTouched] = useState(false)
  const n = value / UNIT_MS[unit]
  const shownNum = Number.isFinite(n) ? String(Math.round(n * 100) / 100) : ''
  const fault = value <= 0 && min !== undefined
    ? `empty · ${label} needs a length; the shortest this accepts is ${len(min)}`
    : min !== undefined && value < min ? `too short · ${len(value)} is under the ${len(min)} minimum`
    : max !== undefined && value > max ? `too long · ${len(value)} is over the ${len(max)} maximum`
    : undefined
  const shown = error ?? (touched ? fault : undefined)
  const bounds = min !== undefined || max !== undefined
    ? min !== undefined && max !== undefined ? `${len(min)} to ${len(max)}` : min !== undefined ? `${len(min)} or longer` : `${len(max!)} or shorter`
    : undefined
  const advice = [hint, bounds].filter(Boolean)
  return (
    <Field label={label} optional={optional} error={shown} id={id} hint={advice.length ? <>{advice.map((a, i) => <span key={i}>{i > 0 && ' · '}{a}</span>)}</> : undefined}>
      {(c) => (
        <span className="block space-y-1.5">
          <span className="flex flex-wrap items-center gap-2">
            <input
              {...c}
              type="number"
              inputMode="numeric"
              min={0}
              step={1}
              disabled={disabled}
              value={shownNum}
              onChange={(e) => onChange(Math.max(0, Number(e.target.value || 0)) * UNIT_MS[unit])}
              onBlur={() => setTouched(true)}
              className={cn(INPUT, 'w-24')}
            />
            <Picker
              label={`${label} unit`}
              value={unit}
              onChange={(u) => { setUnit(u as DurationUnit); onChange(n * UNIT_MS[u as DurationUnit]); setTouched(true) }}
              options={units.map((u) => ({ value: u, meta: UNIT_NAME[u] }))}
              className="h-8 w-28 text-xs"
              width="220px"
            />
          </span>
          {/* The read-back, when it says something the number and the unit do not:
              `90` in `min` is `1h 30m`. `30` in `d` is already `30d`, and
              repeating it would be the same fact twice. */}
          {value > 0 && value % UNIT_MS[unit] !== 0 && <span className="block font-mono text-[11px] text-muted-foreground">{fmtDuration(value)}</span>}
        </span>
      )}
    </Field>
  )
}

/* ── schedule ───────────────────────────────────────────────────────── */

/** 0 is Sunday, as `Date#getDay` counts. */
export type Weekday = 0 | 1 | 2 | 3 | 4 | 5 | 6
const WEEK: { d: Weekday; label: string }[] = [
  { d: 1, label: 'Mo' }, { d: 2, label: 'Tu' }, { d: 3, label: 'We' }, { d: 4, label: 'Th' }, { d: 5, label: 'Fr' }, { d: 6, label: 'Sa' }, { d: 0, label: 'Su' },
]

/** The next `count` runs of `HH:MM` on `days`, so a schedule is verifiable before it is saved. */
export function nextRuns(time: string, days: Weekday[] | undefined, from: Date, count = 3): Date[] {
  const [h, m] = time.split(':').map(Number)
  if (!Number.isFinite(h) || !Number.isFinite(m)) return []
  const out: Date[] = []
  const cursor = new Date(from.getFullYear(), from.getMonth(), from.getDate(), h, m, 0, 0)
  for (let i = 0; i < 400 && out.length < count; i += 1) {
    if (cursor > from && (!days || days.length === 0 || days.includes(cursor.getDay() as Weekday))) out.push(new Date(cursor))
    cursor.setDate(cursor.getDate() + 1)
  }
  return out
}

/**
 * A time of day, its zone, the days it runs on, and the next three runs
 * spelled out underneath. The occurrences are the point: a schedule nobody can
 * read back is a cron expression with extra steps, and `0 4 * * 0` is where a
 * weekly backup quietly becomes a Sunday-only backup. Cron stays available as
 * the advanced entry beside the simple one, never instead of it.
 */
export function ScheduleField({ time, onTimeChange, zone, onZoneChange, zones, days, onDaysChange, next, now, count = 3, cron, onCronChange, label, hint, error, optional, id, disabled }: {
  time: string
  onTimeChange: (t: string) => void
  zone: string
  onZoneChange?: (z: string) => void
  zones?: string[]
  /** Left off, the schedule runs every day. */
  days?: Weekday[]
  onDaysChange?: (d: Weekday[]) => void
  /** Given, these are the occurrences shown; otherwise they are computed from `time`, `days` and `now`. */
  next?: Date[]
  now?: Date
  count?: number
  /** Given with `onCronChange`, an "advanced" text button reveals the cron entry. */
  cron?: string
  onCronChange?: (c: string) => void
  label: string
  hint?: ReactNode
  error?: string
  optional?: boolean
  id?: string
  disabled?: boolean
}) {
  const [touched, setTouched] = useState(false)
  const [advanced, setAdvanced] = useState(false)
  const valid = /^([01]\d|2[0-3]):[0-5]\d$/.test(time)
  const fault = valid ? undefined : `not a time · ${label} is a 24-hour wall clock in ${zone}, e.g. 03:00`
  const shown = error ?? (touched ? fault : undefined)
  const runs = next ?? (valid ? nextRuns(time, days, now ?? new Date(), count) : [])

  return (
    <Field label={label} optional={optional} error={shown} id={id} hint={hint}>
      {(c) => (
        <span className="block space-y-1.5">
          <span className="flex flex-wrap items-center gap-2">
            <input {...c} type="time" value={time} disabled={disabled} onChange={(e) => onTimeChange(e.target.value)} onBlur={() => setTouched(true)} className={cn(INPUT, 'w-28')} />
            {onZoneChange ? (
              <Picker label="time zone" value={zone} onChange={onZoneChange} options={(zones ?? [zone]).map((z) => ({ value: z }))} className="h-8 w-auto min-w-40 text-xs" width="240px" />
            ) : (
              <span className="font-mono text-[11px] text-muted-foreground">{zone}</span>
            )}
          </span>
          {onDaysChange && (
            <Strip
              label="days"
              className="w-max"
              items={WEEK.map((w) => ({
                label: w.label,
                pressed: !!days?.includes(w.d),
                onClick: () => onDaysChange(days?.includes(w.d) ? days.filter((x) => x !== w.d) : [...(days ?? []), w.d].sort((a, b) => a - b)),
              }))}
            />
          )}
          <span className="block font-mono text-[11px] text-muted-foreground">
            {runs.length === 0
              ? 'no runs yet · set a time to see the next three'
              : `next ${runs.map((r, i) => (i === 0 ? `${toStamp(r).replace('T', ' ')} ${zone}` : toStamp(r, { kind: 'date' }))).join(' · ')}`}
          </span>
          {onCronChange && (
            <span className="block space-y-1.5">
              <button type="button" onClick={() => setAdvanced((a) => !a)} className="text-[11px] text-muted-foreground underline underline-offset-4 hover:text-foreground" aria-expanded={advanced}>
                {advanced ? 'hide cron' : 'advanced · cron'}
              </button>
              {advanced && (
                <input
                  aria-label={`${label} cron expression`}
                  value={cron ?? ''}
                  disabled={disabled}
                  placeholder="0 3 * * *"
                  onChange={(e) => onCronChange(e.target.value)}
                  className={cn(INPUT, 'w-44')}
                />
              )}
            </span>
          )}
        </span>
      )}
    </Field>
  )
}
