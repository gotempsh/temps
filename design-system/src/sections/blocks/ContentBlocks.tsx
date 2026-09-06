// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Block, Demo, Rule } from '@/components/op-doc'
import { Callout, PageState, Phrase, EMPTY, fmtAbsolute, fmtBytes, fmtCount, fmtDuration, fmtNum, fmtPct, fmtRelative } from '@/components/op'
import { Button } from '@/components/ui/button'

/* ────────────────────────────────────────────────────────────────────────
   Content — the words, and the functions that keep the numbers honest.
   Reference for docs/content.md and docs/localisation.md. Three blocks:
   the shape of an error, the shape of a time, and what the formatters
   actually print.

   The skin (`operator ink v1`) belongs to the parent page.
   ──────────────────────────────────────────────────────────────────────── */

/** A fixed clock, so the demo prints the same strings on every render. */
const NOW = new Date('2026-09-06T21:14:00Z')
const ago = (ms: number) => new Date(NOW.getTime() - ms)

// ── the shape of an error ─────────────────────────────────────────────

function ErrorGood() {
  return (
    <Callout
      state="error"
      title="Build failed on api-gateway dep_92e"
      quote="src/checkout/AddressForm.tsx:88:31 · Type error: Property 'id' does not exist on type 'Address | undefined'."
      action={<Button size="sm" variant="outline" className="h-7 text-xs">Open the build log</Button>}
    >
      Failed at build container image after 12s. Staging stayed on{' '}
      <Phrase>dep_89f</Phrase>: nothing changed in the environment.
    </Callout>
  )
}

function ErrorBad() {
  return (
    <Callout state="error" title="Error" action={<Button size="sm" variant="outline" className="h-7 text-xs">OK</Button>}>
      Something went wrong. Please try again later or contact support.
    </Callout>
  )
}

// ── the shape of a time ───────────────────────────────────────────────

type Moment = { tag: string; at: Date; what: string }
const MOMENTS: Moment[] = [
  { tag: 'dep_91a', at: ago(41 * 60_000), what: 'switched traffic to api.acme.sh' },
  { tag: 'dep_90e', at: ago(11 * 3_600_000), what: 'superseded, image kept' },
  { tag: 'dep_88c', at: ago(9 * 86_400_000), what: 'cancelled by jules' },
]

function TimeRows() {
  return (
    <ol className="op-rows border text-xs">
      {MOMENTS.map((m) => (
        <li key={m.tag} className="op-row grid grid-cols-[auto_minmax(0,1fr)_auto] items-baseline gap-3">
          <span className="font-mono text-muted-foreground">{m.tag}</span>
          <span className="truncate">{m.what}</span>
          {/* The row says when; the tag says which one; the title says exactly. */}
          <time
            dateTime={m.at.toISOString()}
            title={fmtAbsolute(m.at, { tz: 'UTC', seconds: true, year: true })}
            className="font-mono tabular-nums text-muted-foreground underline decoration-dotted underline-offset-4"
          >
            {fmtRelative(m.at, NOW)}
          </time>
        </li>
      ))}
    </ol>
  )
}

// ── what the formatters print ─────────────────────────────────────────

const OUTPUT: { call: string; out: string }[] = [
  { call: 'fmtNum(30800)', out: fmtNum(30800) },
  { call: "fmtNum(0.6135, { digits: 2 })", out: fmtNum(0.6135, { digits: 2 }) },
  { call: 'fmtPct(0.61)', out: fmtPct(0.61) },
  { call: "fmtPct(31 / 4820, { basis: 'ratio' })", out: fmtPct(31 / 4820, { basis: 'ratio' }) },
  { call: 'fmtBytes(212_000_000)', out: fmtBytes(212_000_000) },
  { call: 'fmtBytes(1536, { binary: true })', out: fmtBytes(1536, { binary: true }) },
  { call: 'fmtDuration(812)', out: fmtDuration(812) },
  { call: 'fmtDuration(2_472_000)', out: fmtDuration(2_472_000) },
  { call: 'fmtDuration(7_500_000)', out: fmtDuration(7_500_000) },
  { call: 'fmtRelative(41m ago, now)', out: fmtRelative(ago(41 * 60_000), NOW) },
  { call: 'fmtRelative(9d ago, now)', out: fmtRelative(ago(9 * 86_400_000), NOW) },
  { call: "fmtAbsolute(t, { tz: 'UTC' })", out: fmtAbsolute(NOW, { tz: 'UTC' }) },
  { call: "fmtCount(1, 'deploy')", out: fmtCount(1, 'deploy') },
  { call: "fmtCount(6, 'deploy')", out: fmtCount(6, 'deploy') },
  { call: 'EMPTY', out: EMPTY },
]

function OutputTable() {
  return (
    <ol className="op-rows border font-mono text-[11px]">
      {OUTPUT.map((r) => (
        <li key={r.call} className="op-row grid grid-cols-[minmax(0,1fr)_auto] items-baseline gap-3">
          <span className="truncate text-muted-foreground">{r.call}</span>
          <span className="tabular-nums">{r.out}</span>
        </li>
      ))}
    </ol>
  )
}

// ── the page blocks ───────────────────────────────────────────────────

export const CONTENT_TOC = [
  ['content-error', 'Error messages'],
  ['content-time', 'Time and the id beside it'],
  ['content-fmt', 'fmt — the formatters'],
] as const

export function ContentBlocks() {
  return (
    <>
      <Block
        id="content-error"
        title="Error messages"
        rule={
          <>
            <p>
              One shape: <strong>what failed · on what · why · what to do next</strong>, with the
              identifier. The quote is the other system's own words, in mono, never paraphrased.
            </p>
            <Rule state="ok">Name the operation, the resource and its id in the first line.</Rule>
            <Rule state="ok">Say what did not change, when nothing did.</Rule>
            <Rule state="error">"Something went wrong", "please try again later", "contact support" — a self-hosted operator has nobody to contact.</Rule>
            <Rule state="error">A button that says "OK". The button says what it does.</Rule>
          </>
        }
        api={'what failed · on what · why · what to do next\ndocs/content.md §3'}
      >
        <Demo label="good">
          <ErrorGood />
        </Demo>
        <Demo label="bad">
          <ErrorBad />
        </Demo>
        <Demo label="the same rules as a page state">
          <PageState
            state="error"
            title="Backup db_orders_nightly failed at upload"
            message="S3 endpoint returned 403 AccessDenied"
            resource="backup:bk_7712 · provider:s3-hetzner"
            onRetry={() => {}}
          />
        </Demo>
      </Block>

      <Block
        id="content-time"
        title="Time and the id beside it"
        rule={
          <>
            <p>
              Relative under 24 hours, absolute after, and always beside the id of the thing that
              happened — a time answers "when", the id answers "which one", and an operator needs
              both to say a sentence out loud.
            </p>
            <Rule state="ok">Give every relative time a <code>title</code> with the exact stamp, named zone.</Rule>
            <Rule state="error">"just now". An ISO string in a row. A time with no id.</Rule>
          </>
        }
        api={"fmtRelative(date, now)   // 41m ago · 11h ago · Sep 6 at 20:33\nfmtAbsolute(date, { tz })  // Sep 6 at 21:14 UTC"}
      >
        <Demo label="hover a time to read the exact stamp">
          <TimeRows />
        </Demo>
      </Block>

      <Block
        id="content-fmt"
        title="fmt — the formatters"
        rule={
          <>
            <p>
              The number, date and duration rules of <code>docs/content.md</code>, written once in{' '}
              <code>@temps-sdk/op</code> so no screen has to remember them. Pure functions: no React,
              no state, one locale argument.
            </p>
            <Rule state="ok">Bytes decimal by default, binary where the kernel counts (`MiB`).</Rule>
            <Rule state="ok">Durations in at most two units. Percentages at one decimal.</Rule>
            <Rule state="ok">Nothing is an en dash. Zero is <code>0</code>. Different facts.</Rule>
            <Rule state="error"><code>toFixed</code>, <code>toLocaleString</code> or a hard comma in a screen.</Rule>
          </>
        }
        api={'fmtNum · fmtPct · fmtBytes · fmtDuration\nfmtRelative · fmtAbsolute · fmtCount · EMPTY'}
      >
        <Demo label="output">
          <OutputTable />
        </Demo>
      </Block>
    </>
  )
}
