// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Block, Demo, Rule } from '@/components/op-doc'
import {
  Callout, DateField, DateTimeField, DateTimeRangeField, DurationField, Field, FormErrors, Kbd, ScheduleField, SecretValue, StatusLine,
  type FieldError, type Weekday,
} from '@/components/op'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Toggle } from '../ConsoleV1Admin'

/* ────────────────────────────────────────────────────────────────────────
   Forms and validation (docs/forms.md). A form is a page the reader is
   changing: it owes them the name of every field, whether what they typed is
   wrong, and whether the change is saved. These blocks are the live proof of
   the three: Field anatomy, blur validation with a summary Callout and a
   sticky save bar, and the difference between disabled and unavailable.

   Fictional instance throughout: temps.acme.sh.
   ──────────────────────────────────────────────────────────────────────── */

// ── the live form ──────────────────────────────────────────────────────

type Key = 'name' | 'registry' | 'email' | 'window'
type Values = Record<Key, string>

const START: Values = { name: 'api-gateway', registry: 'ghcr.io/acme', email: 'ops@acme.sh', window: '03:00' }
const SAVED: Values = START

const LABEL: Record<Key, string> = { name: 'project name', registry: 'registry URL', email: 'ACME contact email', window: 'restart window' }

/** Every rule the form knows, in one place, so blur and submit cannot disagree. */
function check(v: Values): Partial<Record<Key, string>> {
  const e: Partial<Record<Key, string>> = {}
  if (!v.name.trim()) e.name = 'empty · a project needs a name before it can be deployed'
  else if (!/^[a-z0-9-]+$/.test(v.name)) e.name = 'not a slug · use lowercase letters, numbers and hyphens'
  if (v.registry.trim() && !v.registry.includes('.')) e.registry = 'not a host · a registry is a hostname and a path, e.g. ghcr.io/acme'
  if (!v.email.trim()) e.email = "empty · Let's Encrypt sends expiry warnings here"
  else if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(v.email)) e.email = 'not an address · certificates will not renew without one that receives mail'
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(v.window)) e.window = 'not a time · 24-hour local time, e.g. 03:00'
  return e
}

const slug = (s: string) => s.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '')

function LiveForm() {
  const [v, setV] = useState<Values>(START)
  const [saved, setSaved] = useState<Values>(SAVED)
  const [touched, setTouched] = useState<Partial<Record<Key, boolean>>>({})
  const [submitted, setSubmitted] = useState(false)
  const [phase, setPhase] = useState<'idle' | 'saving' | 'saved'>('idle')

  const faults = check(v)
  const dirty = (Object.keys(v) as Key[]).some((k) => v[k] !== saved[k])
  // Shown on blur, or on submit for the fields nobody touched. Once a field is
  // in error it re-checks on every keystroke, so the message clears as it is fixed.
  const shown = (k: Key) => ((touched[k] || submitted) ? faults[k] : undefined)
  const listed: FieldError[] = (Object.keys(LABEL) as Key[])
    .filter((k) => submitted && faults[k])
    .map((k) => ({ id: `demo-${k}`, label: LABEL[k], message: faults[k]! }))

  const set = (k: Key) => (next: string) => { setV((p) => ({ ...p, [k]: next })); setPhase('idle') }
  const blur = (k: Key) => () => setTouched((p) => ({ ...p, [k]: true }))

  const save = () => {
    setSubmitted(true)
    if (Object.keys(faults).length) return
    setPhase('saving')
    window.setTimeout(() => { setSaved(v); setSubmitted(false); setPhase('saved') }, 700)
  }

  return (
    <div className="@container border">
      {/* The verdict, not a toast: it is still true after the toast would have gone.
          StatusLine bleeds to its container's edge, so it sits outside the padded body. */}
      {phase === 'saved' && !dirty && (
        <div className="px-4 sm:px-6"><StatusLine state="ok" sticky={false}>Saved. The restart window applies from tonight.</StatusLine></div>
      )}
      <div className="space-y-4 p-4">
        <FormErrors errors={listed} />
        <Field id="demo-name" label={LABEL.name} error={shown('name')}
          hint={<>served at <span className="font-mono">{slug(v.name) || '—'}.temps.acme.sh</span> · takes effect now</>}>
          <Input id="demo-name" value={v.name} onChange={(e) => set('name')(e.target.value)} onBlur={blur('name')}
            aria-describedby="demo-name-hint demo-name-error" aria-invalid={shown('name') ? true : undefined}
            className="h-8 font-mono text-xs" placeholder="api-gateway" />
        </Field>
        <Field optional id="demo-registry" label={LABEL.registry} error={shown('registry')} hint="off: images stay on the node that built them · takes effect now">
          {(c) => <Input {...c} value={v.registry} onChange={(e) => set('registry')(e.target.value)} onBlur={blur('registry')} className="h-8 font-mono text-xs" placeholder="ghcr.io/acme" />}
        </Field>
        <Field id="demo-email" label={LABEL.email} error={shown('email')} hint="Let's Encrypt sends expiry warnings here · takes effect now">
          {(c) => <Input {...c} value={v.email} onChange={(e) => set('email')(e.target.value)} onBlur={blur('email')} className="h-8 font-mono text-xs" placeholder="ops@acme.sh" />}
        </Field>
        <Field id="demo-window" label={LABEL.window} error={shown('window')} hint="local time on the node · takes effect at the next release">
          {(c) => <Input {...c} value={v.window} onChange={(e) => set('window')(e.target.value)} onBlur={blur('window')} className="h-8 w-24 font-mono text-xs" placeholder="03:00" />}
        </Field>
      </div>
      {/* The one save on the page. Dirty says so, discard sits beside it, and the button carries its own progress. */}
      <div className={`flex items-center gap-3 border-t px-4 py-2 text-xs ${dirty ? '' : 'text-muted-foreground'}`}>
        <span>{phase === 'saving' ? 'saving…' : dirty ? 'unsaved changes' : 'no changes'}</span>
        {dirty && phase !== 'saving' && (
          <button type="button" className="underline underline-offset-4 hover:text-foreground"
            onClick={() => { setV(saved); setTouched({}); setSubmitted(false); setPhase('idle') }}>discard</button>
        )}
        <Button size="sm" disabled={!dirty || phase === 'saving'} onClick={save} className="op-primary ml-auto h-8 text-xs">
          {phase === 'saving' ? 'saving…' : <>save <Kbd keys={['⌘', 'S']} className="ml-1 opacity-70" /></>}
        </Button>
      </div>
    </div>
  )
}

/** A stored secret is never prefilled into an input: it is shown as set, revealable, copyable. */
function SecretValueDemo() {
  const [revealed, setRevealed] = useState(false)
  return <SecretValue value="ghp_9f31c8ad2b7e4c60a1d5e8" secret revealed={revealed} onToggle={() => setRevealed((r) => !r)} />
}

// ── disabled vs unavailable ────────────────────────────────────────────

function DisabledPair() {
  return (
    <div className="grid gap-4 @2xl:grid-cols-2">
      <div className="border">
        <div className="border-b px-4 py-2"><p className="op-label">disabled, with the reason</p></div>
        <div className="@container space-y-4 p-4">
          <Field label="shared memory" hint="set when the database is created; make a new database to change it">
            <Input readOnly disabled value="256 MB" className="h-8 w-32 font-mono text-xs" />
          </Field>
          <Field label="point-in-time recovery" hint="MariaDB 10.11 has no binlog on this instance; upgrade to 11.4 to turn it on">
            <Toggle checked={false} disabled onChange={() => {}} />
          </Field>
        </div>
      </div>
      <div className="border">
        <div className="border-b px-4 py-2"><p className="op-label">unavailable, so it onboards</p></div>
        <div className="@container space-y-4 p-4">
          <Callout state="idle" title="No S3 bucket is configured"
            action={<Button size="sm" variant="outline" className="h-7 text-xs">configure storage</Button>}>
            With one, nightly backups of <span className="font-mono">orders-db</span> upload here and the last 30 are restorable from this page. Settings → storage.
          </Callout>
          <Field label="backup destination" hint="the bucket nightly dumps are written to">
            <Input placeholder="s3://acme-backups/temps" className="h-8 font-mono text-xs" />
          </Field>
        </div>
      </div>
    </div>
  )
}

// ── dates, times, ranges ───────────────────────────────────────────────

/* A fixed clock, not `new Date()`: these blocks are screenshot every run, and a
   demo whose "next three runs" move overnight is a baseline that fails by
   Tuesday. The instance's zone is UTC and its operator is in Europe/Madrid. */
const NOW = new Date(2026, 8, 6, 20, 33, 0)
const ZONES = ['UTC', 'Europe/Madrid', 'America/New_York']
/** The 7-day PITR window acme-pg keeps, as the two bounds the field refuses outside of. */
const PITR_FLOOR = '2026-08-30T20:33:00'
const PITR_CEIL = '2026-09-06T20:33:00'

function PointInTime() {
  const [at, setAt] = useState('2026-09-06T19:04:00')
  const [zone, setZone] = useState('UTC')
  const [window, setWindow] = useState('09:00')
  const [expires, setExpires] = useState('2026-12-06')
  const [never, setNever] = useState(false)
  return (
    <div className="@container space-y-4 border p-4">
      <DateTimeField
        id="demo-pitr" label="restore acme-pg to" precision="second" zone={zone} onZoneChange={setZone} zones={ZONES}
        value={at} onChange={setAt} min={PITR_FLOOR} max={PITR_CEIL}
        hint="any second in the last 7 days · WAL is replayed to it"
        presets={[
          { label: 'now', value: PITR_CEIL },
          { label: '−1h', value: '2026-09-06T19:33:00' },
          { label: 'last backup', value: '2026-09-06T18:33:00' },
        ]}
      />
      <DateField
        id="demo-expiry" label="ci-deploy expires" zone={zone} value={expires} onChange={setExpires} min="2026-09-07" now={NOW}
        hint="CI deploys stop when it does"
        never={{ label: 'no expiry', on: never, onChange: setNever }}
      />
      <ScheduleField
        id="demo-window" label="restart window" time={window} onTimeChange={setWindow} zone={zone} now={NOW} count={1}
        hint="self-update restarts inside this window"
      />
    </div>
  )
}

function RangeAndSchedule() {
  const [win, setWin] = useState({ from: '2026-09-05T20:33:00', to: '2026-09-06T20:33:00' })
  const [gate, setGate] = useState<string | null>(null)
  const [time, setTime] = useState('02:00')
  const [days, setDays] = useState<Weekday[]>([1, 2, 3, 4, 5])
  const [cron, setCron] = useState('0 2 * * 1-5')
  const [keep, setKeep] = useState(30 * 86_400_000)
  const [timeout, setTimeout_] = useState(90 * 60_000)
  return (
    <div className="@container space-y-4 border p-4">
      <DateTimeRangeField
        id="demo-range" label="proxy logs" zone="UTC" now={NOW}
        from={win.from} to={win.to} onChange={(from, to) => setWin({ from, to })}
        min="2026-08-07T20:33:00" max="2026-09-06T20:33:00"
        quick={[{ label: '1h', hours: 1 }, { label: '24h', hours: 24 }, { label: '7d', hours: 168 }, { label: '30d', hours: 720 }, { label: '90d', hours: 2160 }]}
        retentionDays={30} retentionLabel="30d" onGated={(q) => setGate(`${q.label} is beyond 30d retention on Starter · Team keeps 90d`)}
        hint="the window the log ledger reads"
      />
      {gate && <Callout state="idle" title="90d is beyond this plan's retention">Starter keeps 30 days of proxy logs. Team keeps 90. Nothing older than 2026-08-07 20:33 UTC exists to read.</Callout>}
      <ScheduleField
        id="demo-schedule" label="nightly-all runs at" time={time} onTimeChange={setTime} zone="UTC" now={NOW}
        days={days} onDaysChange={setDays} cron={cron} onCronChange={setCron}
        hint="every service plus the control plane"
      />
      <DurationField id="demo-keep" label="keeps backups for" value={keep} onChange={setKeep} units={['h', 'd']} min={86_400_000} max={365 * 86_400_000} hint="older ones are deleted on the next nightly pass" />
      {/* 90 minutes is where the read-back earns its line: the number and the unit do not say `1h 30m` on their own. */}
      <DurationField id="demo-timeout" label="build timeout" value={timeout} onChange={setTimeout_} units={['min', 'h']} min={60_000} max={6 * 3_600_000} hint="a build still running at this point is killed" />
    </div>
  )
}

// ── the section ────────────────────────────────────────────────────────

export function FormBlocks() {
  return (
    <>
      <Block id="form-field" title="Field" api={`<Field
  label="ACME contact email"
  hint="expiry warnings go here"
  error="not an address · certificates will not renew"
  optional
  id="acme-email">
  {(c) => <Input {...c} />}
</Field>`}
        rule={<>
          <p>Label at 500 and always visible, hint, control, error line. A placeholder is an example, never a name.</p>
          <p>The error renders under the hint as glyph + sentence in the destructive tone: the only colour a field carries. The hint stays put while it shows, because advice and fault are different things.</p>
          <p>Pass <code>id</code>, or take the render-prop form, and the control is wired: <code>aria-describedby</code> reaches the hint and the error, <code>aria-invalid</code> is set while the error is, and neither is folded into the control's name.</p>
          <Rule state="ok">optional is marked, because the console's fields are mostly required.</Rule>
          <Rule state="error">"Invalid input" — a message that fits every field explains none of them.</Rule>
        </>}>
        <Demo label="anatomy">
          <div className="@container space-y-4 border p-4">
            <Field label="external URL" hint="what links, webhooks and OAuth callbacks use · takes effect now">
              <Input defaultValue="https://temps.acme.sh" className="h-8 font-mono text-xs" />
            </Field>
            <Field optional label="registry URL" hint="off: images stay on the node that built them">
              <Input placeholder="ghcr.io/acme" className="h-8 font-mono text-xs" />
            </Field>
            <Field label="ACME contact email" hint="Let's Encrypt sends expiry warnings here"
              error="not an address · certificates will not renew without one that receives mail">
              {(c) => <Input {...c} defaultValue="ops" className="h-8 font-mono text-xs" />}
            </Field>
            <Field label="registry password" hint="stored encrypted; services keep the old one until they are redeployed">
              <SecretValueDemo />
            </Field>
          </div>
        </Demo>
      </Block>

      <Block id="form-validation" title="Validation and save" api={`// blur validates a field, submit validates the form,
// a field already in error re-checks on every keystroke
<FormErrors errors={[{ id, label, message }]} />
// sticky bar: unsaved changes · discard · save ⌘S`}
        rule={<>
          <p>Validate a field when it loses focus, the whole form on submit, and a field already in error on every keystroke so the message clears as it is fixed. Never on the first keystroke — a live slug preview is a format hint, not a verdict.</p>
          <p>More than one field failing adds a <code>FormErrors</code> Callout at the top; each entry moves DOM focus to its field, and the inline message stays where it is. One field failing is already marked in place.</p>
          <p>The save is the sticky bar: dirty says so and offers discard, the button carries its own progress, and the confirmation is the page's verdict rather than a toast that will be gone before it is read.</p>
          <Rule state="ok">Leave "ACME contact email" empty and press save: the summary lists it and focuses it.</Rule>
          <Rule state="error">A save button disabled because the form is invalid. It explains nothing.</Rule>
        </>}>
        <Demo label="live · blur to validate, save to submit">
          <LiveForm />
        </Demo>
      </Block>

      <Block id="form-datetime" title="Dates and times" api={`<DateTimeField
  label="restore acme-pg to"
  value={at} onChange={setAt}
  zone={zone} onZoneChange={setZone}
  precision="second"
  min={floor} max={ceil}
  presets={[{ label: 'now', value: ceil }]} />

<DateField never={{ label: 'no expiry', on, onChange }} />`}
        rule={<>
          <p>Typed entry first: a real <code>datetime-local</code> under the ink skin, so a stamp copied out of the failing log line can be pasted, <code>↑</code>/<code>↓</code> step the focused segment, and the browser's picker is the accelerator rather than the only door.</p>
          <p>The zone sits beside the control as a fact, always — a <code>Picker</code> in the same <code>Field</code> when it can be changed. Three clocks are in play on any restore (the operator's, the node's, the bucket's) and a control that guesses picks the wrong second silently.</p>
          <p>The window is stated once in the hint and refused outside it on blur, with the state word and the fact. Presets fill the absolute field; the field stays the truth about what they wrote. "no expiry" is an option word, never an empty date.</p>
          <Rule state="ok">Type 2026-08-20 into the restore field and leave it: out of window, with both bounds named.</Rule>
          <Rule state="error">A calendar icon as the only entry, or <code>09/06/26</code> with no zone beside it.</Rule>
        </>}>
        <Demo label="live · type, blur, press a preset">
          <PointInTime />
        </Demo>
      </Block>

      <Block id="form-range-schedule" title="Ranges, schedules and durations" api={`<DateTimeRangeField from to onChange zone
  quick={[{ label: '7d', hours: 168 }]}
  retentionDays={30} retentionLabel="30d" onGated />

<ScheduleField time zone days onDaysChange cron />
<DurationField value={ms} onChange units={['h','d']} />`}
        rule={<>
          <p>A range is two fields, <code>from</code> → <code>to</code>, on the strip a chart's <code>RangePicker</code> already uses. <code>to &gt; from</code> is checked on blur of "to"; a window past the retention horizon is struck through with the plan word and says what keeps it, never hidden.</p>
          <p>A schedule prints its next three runs underneath, so it is verifiable before it is saved — <code>0 4 * * 0</code> is where a weekly backup quietly becomes a Sunday-only backup. Cron stays available beside the simple entry, never instead of it.</p>
          <p>A duration is a number and a unit <code>Picker</code>: <code>30d</code>, <code>30 days</code>, <code>720h</code> and <code>30</code> are four spellings of one value and three of them are a parser bug. The preview under it is <code>fmtDuration</code>.</p>
          <Rule state="ok">Press 90d: it is struck through and explains which plan keeps it.</Rule>
          <Rule state="error">A free-text "30d" box, or a schedule that shows only the cron string.</Rule>
        </>}>
        <Demo label="live · press 90d, toggle a weekday, change the unit">
          <RangeAndSchedule />
        </Demo>
      </Block>

      <Block id="form-disabled" title="Disabled and unavailable" api={`// disabled  → the reason sits beside it, in the hint
// unavailable → Callout: what is missing, what it would
//               do, and the link that configures it`}
        rule={<>
          <p>A control the reader cannot use owes them the reason in the same breath. "Set at creation", "needs 11.4": both are facts they can act on.</p>
          <p>A control that needs operator configuration onboards instead of disappearing: say what is missing, show a concrete example of what it will do, link the settings page that fixes it. Hiding it means the feature does not exist.</p>
          <Rule state="ok">Both halves render. Neither is a dead grey box.</Rule>
          <Rule state="error">A greyed toggle with no explanation, or a section that renders nothing until a bucket exists.</Rule>
        </>}>
        <Demo label="pair">
          <DisabledPair />
        </Demo>
      </Block>
    </>
  )
}
