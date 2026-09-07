// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Block, Demo, Rule } from '@/components/op-doc'
import {
  AgentQuestion, AgentSources, Proposal, Provenance, StreamBlock, TimeChart, ToolRow,
  Ledger, Status, type LedgerRow, type Series, type State, type TimePoint,
} from '@/components/op'

/**
 * Generative UI: what an AI is allowed to render, and what proves it.
 * Companion to `docs/generative-ui.md`. The working surface is `/agent`;
 * these blocks are the states that page cannot show all at once.
 *
 * Everything here is fixture data with fictional names.
 */

// ── fixtures ───────────────────────────────────────────────────────────

const HOURS = ['06:00', '06:30', '07:00', '07:30', '08:00', '08:30', '09:00', '09:30', '10:00', '10:30', '11:00', '11:30']
const RATE = [0.2, 0.3, 0.2, 0.4, 0.3, 0.5, 6.1, 8.4, 9.2, 8.8, 9.6, 9.1]
const POINTS: TimePoint[] = HOURS.map((t, i) => ({ t, rate: RATE[i] }))
const SERIES: Series[] = [{ key: 'rate', name: 'errors / min', state: 'error' }]

const QUERY = `temps get_error_time_series
  project      checkout-web
  environment  production
  from         2026-09-07T06:00
  to           2026-09-07T11:45
  bucket       30m`

const GROUPS: { id: string; title: string; where: string; events: string; state: State }[] = [
  { id: 'err_4f21', title: "TypeError: cannot read 'line1'", where: 'AddressForm.tsx:87', events: '1,204', state: 'error' },
  { id: 'err_39c8', title: 'NotFound: order not found', where: 'orders.ts:88', events: '96', state: 'error' },
  { id: 'err_1a04', title: 'FetchError: ETIMEDOUT registry', where: 'build', events: '11', state: 'warn' },
]

const GROUP_ROWS: LedgerRow[] = GROUPS.map((g) => ({
  id: g.id,
  state: g.state,
  cells: [
    <span key="t" className="min-w-0 truncate">{g.title}</span>,
    <span key="w" className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{g.where}</span>,
    <Status key="e" state={g.state} label={g.events} className="font-mono" />,
  ],
  mobile: <span className="min-w-0"><span className="block truncate">{g.title}</span><span className="block truncate font-mono text-[11px] text-muted-foreground">{g.events} events · {g.where}</span></span>,
}))

function ErrorLedger() {
  return (
    <Ledger
      status={null}
      dense
      columns={['error', 'where', { label: 'events', key: 'events', numeric: true }]}
      grid="1.6fr 1.1fr 100px"
      rows={GROUP_ROWS}
      total={GROUPS.length}
      hint="most events first"
    />
  )
}

// ── wrong / right pair ─────────────────────────────────────────────────

/**
 * The thing the rules replace, hand-drawn so the package cannot be used to
 * make it: a bubble with an avatar, a markdown table where a Ledger exists,
 * and a number with nothing behind it.
 */
function ChatBubbleAnswer() {
  return (
    <div className="flex gap-2">
      <span aria-hidden className="mt-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] font-semibold">AI</span>
      <div className="min-w-0 rounded-2xl bg-muted px-3 py-2 text-sm">
        <p>I have successfully analysed your error data! 🎉 Here is what I found:</p>
        <pre className="mt-2 overflow-x-auto whitespace-pre font-mono text-[11px] leading-5">{`| Error                        | Where               | Events |
|------------------------------|---------------------|--------|
| TypeError: cannot read line1 | AddressForm.tsx:87  | 1204   |
| NotFound: order not found    | orders.ts:88        | 96     |
| FetchError: ETIMEDOUT        | build               | 11     |`}</pre>
        <p className="mt-2">Your error rate increased by roughly 2000%. Let me know if you would like me to roll back — I can do that for you automatically!</p>
      </div>
    </div>
  )
}

// ── the page blocks ────────────────────────────────────────────────────

export const GENUI_TOC = [
  ['genui-ledger', 'The ledger of tool calls'],
  ['genui-provenance', 'Provenance'],
  ['genui-proposal', 'Proposals'],
  ['genui-streaming', 'Streaming states'],
  ['genui-question', 'Asking'],
  ['genui-wrong', 'The bubble it replaces'],
] as const

export function GenUiBlocks() {
  const [answer, setAnswer] = useState<string | null>(null)
  const [reversible, setReversible] = useState<'confirmed' | 'declined' | null>(null)
  const [irreversible, setIrreversible] = useState<'confirmed' | 'declined' | null>(null)
  const [gate, setGate] = useState<'waiting' | 'approved' | 'denied'>('waiting')
  return (
    <>
      <Block
        id="genui-ledger"
        title="The ledger of tool calls"
        rule={
          <>
            <p>
              An agent's work is <strong>a ledger of typed calls</strong>, not a paragraph about them. One row per call:
              kind icon · name and argument in mono · the state word · the duration. Collapsed by default; open for the
              input and the output.
            </p>
            <Rule state="ok">A failing call is <span className="font-mono">×  failed</span> and its error sentence, always visible.</Rule>
            <Rule state="error">Hiding a call, summarising six calls as “I looked into it”, or a spinner with no name.</Rule>
          </>
        }
        api={`<ToolRow
  name="query_metrics" arg="checkout-web · 7d"
  state="output-available"   // 6 states + 2 approval outcomes
  ms={412} input={json} output={text} diff={patch} error={line}
  approval={{ reason, destructive, onRespond }} approved
/>`}
      >
        <Demo label="every state">
          <div className="space-y-1">
            <ToolRow name="query_metrics" arg="checkout-web · production · 7d" state="input-streaming" />
            <ToolRow name="query_logs" arg="checkout-web · since 09:00" state="input-available" ms={2140} />
            <ToolRow name="list_error_groups" arg="checkout-web · since dep_91a" state="output-available" ms={188} defaultOpen={false} output={'4 groups · 1,315 events'} />
            <ToolRow name="get_container_logs" arg="checkout-web · fsn1-3" state="output-error" error={'403 Forbidden · logs are not readable with the deployment token in use.\nGrant "read logs" on Settings → API keys, then ask again.'} />
            <ToolRow name="read_file" arg="src/checkout/address.ts" state="output-available" ms={38} defaultOpen={false} input={'{ "path": "src/checkout/address.ts", "lines": "1-40" }'} output={'40 lines · 2 kB'} />
          </div>
        </Demo>
        <Demo label="a call that waits · Y approves, N denies">
          <ToolRow
            name="restart_container" arg="checkout-web · production · fsn1-3"
            state={gate === 'waiting' ? 'approval-requested' : gate === 'denied' ? 'output-denied' : 'output-available'}
            ms={9400} approved={gate === 'approved'}
            output={'restarted · healthy at 11:52 · 1 request dropped'}
            approval={{ reason: 'Restarts one container of three. Traffic drains first; the other two serve during the ~9s gap.', onRespond: (r) => setGate(r === 'deny' ? 'denied' : 'approved') }}
          />
        </Demo>
      </Block>

      <Block
        id="genui-provenance"
        title="Provenance"
        rule={
          <>
            <p>
              Every generated block carries <strong>the call that produced it</strong>, as one mono line under it:
              <span className="font-mono"> from query_metrics · 41m ago · 7d</span>, with the query itself one click away.
            </p>
            <Rule state="ok">The reader can always get from a picture back to the rows it was drawn from.</Rule>
            <Rule state="error">A chart, a list or a number an agent rendered with no source line. Banned outright.</Rule>
          </>
        }
        api={`<Provenance tool="temps get_error_time_series" when="2m ago"
  range="5h45m · 30m buckets" note="checkout-web · production"
  query={sent}>
  <TimeChart … />
</Provenance>`}
      >
        <Demo label="a chart and its source">
          <Provenance tool="temps get_error_time_series" when="2m ago" range="5h45m · 30m buckets" note="checkout-web · production" query={QUERY}>
            <TimeChart
              data={POINTS} series={SERIES} unit="/min"
              title="checkout-web error rate" range="06:00 → 11:45 · 30m buckets"
              verdict="Flat at 0.4/min until 09:00, then 9.1/min from dep_91a onward."
              markers={[{ id: 'dep_91a', x: '09:00', at: '09:00' }]}
              thresholds={[{ y: 2, label: 'budget 2/min', state: 'warn' }]}
            />
          </Provenance>
        </Demo>
        <Demo label="a list and its source">
          <Provenance tool="temps list_error_groups" when="2m ago" range="since dep_91a" note="3 of 3 groups" query={'temps list_error_groups\n  project      checkout-web\n  since        dep_91a\n  order_by     events desc'}>
            <ErrorLedger />
          </Provenance>
        </Demo>
        <Demo label="what was read">
          <AgentSources items={[
            { label: 'err_4f21', href: '#', note: 'error tracking · 1,204 events' },
            { label: 'dep_91a', href: '#', note: 'deployment · 09:00' },
            { label: 'dep_90c', href: '#', note: 'deployment · last healthy' },
          ]} />
        </Demo>
      </Block>

      <Block
        id="genui-proposal"
        title="Proposals"
        rule={
          <>
            <p>
              Reads render. Writes are <strong>proposed, never performed</strong>: the action, the target, the consequence,
              whether it can be undone, and the autonomy level the capability runs at — then confirm or decline.
            </p>
            <Rule state="ok">Reversible asks in ink. Irreversible goes through <span className="font-mono">EchoDialog</span> and types the name.</Rule>
            <Rule state="error">An agent confirming its own proposal, red on something you can undo, or a write with no consequence sentence.</Rule>
          </>
        }
        api={`<Proposal action="roll back" target="checkout-web → dep_90c"
  consequence="Production serves dep_90c in ~40s…"
  reversal="Reversible: redeploy dep_91a."
  irreversible={false} autonomy="act with approval"
  decided={decision} onConfirm={…} onDecline={…} />`}
      >
        <Demo label="reversible · asks in ink">
          <Proposal
            kind="deploy"
            action="roll back"
            target="checkout-web · production → dep_90c"
            consequence="Production serves dep_90c again within about 40 seconds; the 9.1/min error rate should return to 0.4/min."
            reversal="Reversible: redeploy dep_91a from the deployment page, or ask me to."
            autonomy="act with approval · rollback is on the write allowlist for this project"
            confirmLabel="roll back to dep_90c"
            decided={reversible}
            onConfirm={() => setReversible('confirmed')}
            onDecline={() => setReversible('declined')}
          />
        </Demo>
        <Demo label="irreversible · red, and the name is typed">
          <Proposal
            kind="database"
            action="delete"
            target="staging-copy"
            consequence="Deletes the database staging-copy and its 2.1 GB of data. It has no backup and no replica."
            reversal="This cannot be undone by you, by me, or by support."
            irreversible
            autonomy="act with approval · deletes always ask, whatever the mode"
            confirmWord="staging-copy"
            confirmLabel="delete staging-copy"
            steps={['drain connections', 'drop the database', 'release the volume']}
            decided={irreversible}
            onConfirm={() => setIrreversible('confirmed')}
            onDecline={() => setIrreversible('declined')}
          />
        </Demo>
      </Block>

      <Block
        id="genui-streaming"
        title="Streaming states"
        rule={
          <>
            <p>
              Four states and nothing else: <strong>thinking</strong> with the seconds so far, a <strong>call running</strong>
              with a live duration, <strong>partial text</strong> with a caret, and a <strong>skeleton shaped like the block
              that is coming</strong>, so the page does not jump when it lands.
            </p>
            <Rule state="ok">On stop, what completed stays and what did not says so: “Stopped after step 4 of 9.”</Rule>
            <Rule state="error">A shimmer, a chart that draws itself, a spinner as the whole answer, or a block that appears at a different height than its skeleton.</Rule>
          </>
        }
        api={`<StreamBlock kind="chart" />   // text · chart · ledger
<StreamBlock kind="ledger" />        // detail · keyvalue · tool
<ToolRow state="input-available" ms={elapsed} />`}
      >
        <Demo label="thinking, and a call running">
          <div className="space-y-1">
            <ToolRow name="thinking" kind="reasoning" state="input-streaming" meta={<>6s<span className="op-caret" /></>} />
            <ToolRow name="query_traces" arg="checkout-web · 09:00→11:45" state="input-available" ms={2140} />
          </div>
        </Demo>
        <Demo label="partial text">
          <StreamBlock kind="text" label="writing the verdict" />
        </Demo>
        <Demo label="a chart on its way">
          <StreamBlock kind="chart" label="drawing checkout-web error rate · 12 buckets" />
        </Demo>
        <Demo label="a list on its way">
          <StreamBlock kind="ledger" label="listing error groups since dep_91a" />
        </Demo>
      </Block>

      <Block
        id="genui-question"
        title="Asking"
        rule={
          <>
            <p>
              When the agent needs a decision it asks with <strong>typed options</strong>, two to four, each with the
              consequence of picking it. Answering is pick then confirm — one click never sends an answer mid-run.
            </p>
            <Rule state="ok">Missing parameters are a form of <span className="font-mono">Field</span>s, not a sentence asking for them.</Rule>
            <Rule state="error">“Please provide the environment name and time range.” Free text where an option list exists.</Rule>
          </>
        }
        api={`<AgentQuestion q="…" answer={a} onAnswer={setA}
  options={[{ label, note }, …]} />   // 1–4 pick, ⏎ confirms`}
      >
        <Demo label="typed options">
          <AgentQuestion
            q="Two projects are called checkout. Which one do you mean?"
            answer={answer}
            onAnswer={setAnswer}
            options={[
              { label: 'checkout-web', note: 'app · production · 1,204 errors since 09:00' },
              { label: 'checkout-worker', note: 'worker · production · no errors today' },
            ]}
          />
        </Demo>
      </Block>

      <Block
        id="genui-wrong"
        title="The bubble it replaces"
        rule={
          <>
            <p>
              The same answer, twice. The first is what every assistant ships: an avatar, a bubble, a markdown table,
              a percentage with nothing behind it, and an offer to act by itself.
            </p>
            <Rule state="error">Avatar · bubble · “I have successfully…” · emoji · a table where a <span className="font-mono">Ledger</span> exists · an unsourced number · an agent volunteering to write.</Rule>
            <Rule state="ok">Verdict first, then the blocks the console already has, each with its source, and the write as a proposal that waits.</Rule>
          </>
        }
      >
        <Demo label="wrong">
          <ChatBubbleAnswer />
        </Demo>
        <Demo label="right">
          <div className="space-y-2">
            <p className="max-w-[68ch] text-sm leading-6">
              <strong>Roll back checkout-web production to dep_90c.</strong> The error rate went from 0.4 to 9.1 per minute
              at 09:00, when <span className="font-mono">dep_91a</span> shipped; 1,204 of the 1,315 events since are
              <span className="font-mono"> err_4f21</span>.
            </p>
            <ToolRow name="list_error_groups" arg="checkout-web · since dep_91a" state="output-available" ms={188} defaultOpen={false} output={'3 groups · 1,311 events'} />
            <Provenance tool="temps list_error_groups" when="2m ago" range="since dep_91a" note="3 of 3 groups" query={'temps list_error_groups\n  project      checkout-web\n  since        dep_91a'}>
              <ErrorLedger />
            </Provenance>
          </div>
        </Demo>
      </Block>
    </>
  )
}
