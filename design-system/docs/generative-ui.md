# Generative UI

The system for anything an AI renders — the console assistant, a coding agent,
a scheduled agent writing a finding. Companion to `brand-guidelines.md` §0
("AI-native, under policy"), `design-system-handoff.md` §7b ("Agent
conversation"), `forms.md` and `notifications.md`. Rules digest:
`RULES.md` §"Generative UI". Reference implementation: `/agent` (two
scenarios: coding agent · console assistant) and `/op-components#genui-ledger`.

Primitives: `ToolRow`, `Proposal`, `Provenance`, `StreamBlock`,
`AgentQuestion`, `AgentSources`, `RunAside` in `@temps-sdk/op`
(`web/packages/op/src/agent.tsx`).

---

## 1. What generative UI is here

An agent answers with **the console's own blocks**, not with prose describing
them. It has no drawing kit of its own.

- Render a value over time as `TimeChart`. Not a markdown table of hours, not
  an ASCII sparkline, not an image.
- Render a set of records as `Ledger`, one record as `Detail`, facts about one
  thing as `KeyValue`, a share or a rank as `Breakdown`, logs as `LogLines`.
- Pick the block with the same table everyone else uses: `data-viz.md` §1 for
  charts, `RULES.md` §"Page structure" for layout. The agent gets no exception.
- **Nothing an agent renders may be a component the console does not have.** If
  the answer needs a block that does not exist, the answer is text plus the
  rows, and the missing block is a PR.
- Emit colour only through `Status` and the five state glyphs. An agent never
  chooses a hue, a fill, or a highlight; a "risk score" is a word and a glyph.
- Write numbers through `fmt.ts`. A model that formats `1204` as `1,204` in the
  prompt is guessing; `fmtNum` is not.

## 2. The ledger of tool calls

The transcript is a ledger. Every step the agent took is a row, in order.

- Give every call one row: **kind icon · state glyph and word · title · meta ·
  duration** (`ToolRow`). `query_metrics checkout-web · production · 7d` ·
  `done · 412ms`.
- Use the state vocabulary and no other words: `preparing` · `running` ·
  `done` · `failed` · `needs approval` · `approved` · `denied` (`TOOL_STATE`).
- Collapse by default; open to the input and the output. Two exceptions, both
  in the prototype: an edit (the diff *is* the content) and a command (its
  output *is* the content) open by default.
- Show **a failing tool as `×  failed` and its error sentence**, never hidden,
  never collapsed away, never softened into "I had some trouble reading that".
  Quote what the other system said, verbatim, in mono.
- Say when a call needed permission and got it: the meta reads
  `approved · done · 2.1s`. A record that hides the gate is not a record.
- Give the call its real name. The console assistant calls `temps` and
  `temps_write`, each dispatching one allowlisted API operation, so the row
  shows the operation (`get_error_time_series`), which is what the reader can
  look up.
- Never summarise six calls as one sentence. A ledger with rows missing is the
  thing this whole document exists to prevent.

## 3. Provenance

- Hang a source line under **every** generated block, in mono, muted:
  `from query_metrics · 41m ago · 7d` (`Provenance`).
- Name the tool, when it ran, and the window it read. Add one more fact when it
  changes how the block should be read: sampling, retention, `3 of 47 groups`.
- Give it a **show query** affordance that reveals what was actually sent, so a
  disagreement about a number ends in the query, not in an argument.
- Link the block's records into the console: a row opens `err_4f21`, a marker
  opens `dep_91a`.
- **A block with no source is banned.** `Provenance` requires `tool` and `when`
  for exactly this reason: the component cannot be used to launder a picture
  the model invented.
- Do not paraphrase the source in the sentence above the block ("according to
  recent metrics"). The line under it is the citation; one fact once.

## 4. Reading versus writing

- **Reads render immediately.** The read surface is read-only by construction:
  GET operations replayed with the reader's own permissions, so the agent never
  sees what the reader cannot.
- **Writes are proposed, never performed.** A write tool call produces a
  `Proposal` block and stops. Nothing has run until a human confirms.
- State four things in every proposal, in this order: **the action**, **the
  target**, **the consequence** (what changes, for whom, how fast), and
  **whether it can be undone** — and how.
- Confirm a reversible write in ink: `roll back to dep_90c` · `decline`. A
  rollback you can redeploy is not red.
- Route an **irreversible** write through `EchoDialog`: red, the name typed
  out, the steps listed, and a reason that ends in "cannot be undone". Red
  means loss nobody can get back, and nothing else.
- **The agent never confirms its own proposal.** Not with "I'll go ahead", not
  with a countdown, not by pre-selecting the confirm button.
- Show the **autonomy level per capability**, in words, on the proposal and in
  the run aside: `observe` · `propose` · `act with approval` · `autopilot`.
  Never assume it, never infer it from the mode name.
- Say what is not allowed as plainly as what is: `delete anything · not on the
  allowlist` is a fact the reader needs before they ask.
- Say what did not change when nothing did: "Nothing ran. Production stays on
  dep_91a at 9.1 errors per minute."

## 5. Streaming

Four states, and no fifth.

- **Thinking**: one row, `Brain`, with the seconds so far and a caret. It
  collapses afterwards to `thought for 6s · 3/3 steps`.
- **A call running**: the row exists from the moment the call starts, state
  `running`, with the elapsed duration live in the meta.
- **Partial text**: the text as it arrives with `.op-caret` at the end. Never a
  three-dot typing indicator, never a bouncing avatar.
- **A block on its way**: `StreamBlock` holds the shape of the block that is
  coming — chart, ledger, detail, keyvalue, text — at the height it will land
  at, so nothing jumps. Static: a skeleton that shimmers and a chart that draws
  itself are both banned (`RULES.md` §Motion).
- On **stop**: what completed stays on the page, and what did not says so —
  "Stopped after step 4 of 9. Reply to continue." Never wipe the transcript,
  never leave a row `running` forever.
- Keep the reader in control of the scroll: tail while they are at the bottom,
  stop following the moment they scroll up.

## 6. Asking

- Ask with **typed options**, two to four, each with the consequence of picking
  it (`AgentQuestion`). "Two projects are called checkout. Which one?" →
  `checkout-web · app · 1,204 errors since 09:00`.
- Answer in two steps: pick (radio, `1`–`4`), then confirm (`⏎`). One click
  never sends an answer, because a misclick mid-run is not reversible.
- Raise the unanswered question (`.op-raise`) and nothing else on the screen.
  The status line says the agent is waiting and links to it.
- When **parameters are missing**, render a form: one `Field` per parameter,
  visible label, hint, real control (a `Picker` for an environment, a
  `DateTimeRangeField` for a window), validated on blur like every other form.
- **Never a free-text "please provide"**. "Please give me the environment and
  the time range" is a form the agent refused to build.
- Let the reader answer in the composer instead; say so under the options
  ("or type an answer below · the agent waits").

## 7. Evidence and sources

- End an answer with `AgentSources`: what was read, as links into the console's
  own records — `err_4f21 · error tracking · 1,204 events`.
- Link **inside the console**. An external link is allowed only when the tool
  that produced it was a web search, and the row says which.
- Carry evidence, confidence and impact in a finding, or do not show the
  finding: "Found in 31 events since dep_91a, confidence high, verified by the
  checkout suite" is the shape.
- Attribute every number to the call that returned it. A number in the verdict
  must appear in a block below it.

## 8. Errors and limits

Every failure is a state the reader can act on. Self-hosted readers debug
alone.

- **Tool failure**: the row is `×  failed` with the error verbatim, and the
  agent says what it did instead ("Network is off in this worktree; continued
  without it").
- **Permission denied**: name the permission and link the setting that grants
  it — "403 · logs are not readable with this deployment token. Grant *read
  logs* on Settings → API keys." Never "I don't have access to that".
- **Unconfigured capability**: show the surface, say what is missing, give an
  example of what it would do, link the settings page. A capability endpoint
  answers `configured: false` with a reason and a setup path, so the client can
  tell "not built" from "not set up".
- **Context limit**: the `ContextBadge` states tokens and percent, tints at
  75%, and offers `compact now`; the transcript takes a checkpoint before it
  compacts, and restoring a checkpoint is an `EchoDialog` because it throws
  work away.
- **Rate limit or quota**: state the fact and the retry — "Model rate limit
  reached · retrying in 20s · 3 of 5 attempts". Never a bare spinner.
- **Budget**: an agent that would exceed its budget stops and says the number,
  it does not silently downgrade the model.

## 9. Layout

- The **conversation is the main column** and the only scrolling one.
- The **aside is the run** (`RunAside`, 280px at xl): model · workspace ·
  permission mode · context · checkpoints, as `KeyValue`, plus the autonomy
  list and whatever this run needs stated (tools, proposals, cost).
- The **composer is fixed at the bottom** (`.op-sticky-bottom`): textarea, then
  a row of `Picker`s that say model, thinking, mode and workspace in words, the
  context badge, and the one ink fill (send) — replaced by `stop` only while
  the agent is actually executing.
- **Below md the aside collapses into the composer's picker row.** The facts do
  not disappear; they become the pickers that already say them.
- Turns carry who · when · model in a left column at 88px, which stacks into a
  line on a phone. There are no boxes inside a turn.
- One raised element on the screen, and it is whatever is waiting on the
  reader: the unanswered question, or the pending proposal.

## 10. Voice

The agent writes like the console (`content.md`).

- **Verdict first, then the blocks.** "Roll back checkout-web production to
  dep_90c." then the chart, the list, the proposal.
- Sentence case. One term per concept. `roll back` the verb, `rollback` the
  noun. No exclamation marks, no emoji, no "Great question".
- **One fact once.** If the number is in the chart, the sentence names what it
  means, not the number again.
- Never "I have successfully…", "I've analysed your data", "Let me know if
  you'd like me to". State what happened and what is next.
- Quote machines verbatim in mono and translate nothing they wrote.
- Say the id beside every time (`41m ago · dep_91a`), and give every relative
  time its absolute stamp as a title.
- Name the human as the governor: "Temps prepares the change. You review and
  approve it." Never "let AI handle it".

## 11. Banned

- Chat bubbles, avatars, an "AI" circle, typing dots, a bouncing anything.
- A markdown table where a `Ledger`, `KeyValue` or `Breakdown` exists.
- A chart, list or number with no `Provenance`.
- An agent confirming its own proposal, or a write that ran without a human.
- A sparkle, a wand, a gradient, the word "magic". AI is `Bot` and `Brain`.
- A colour an agent chose. Colour is `Status`: glyph, word, tone.
- A hidden, collapsed-away or summarised-over tool call, especially a failed one.
- A free-text "please provide …" where typed options or a `Field` belong.
- A shimmer, a self-drawing chart, a counting number, a spinner as the answer.
- "I have successfully", "Something went wrong", "please try again later".
- An external link from a tool that was not a web search.
- A block whose skeleton was a different shape or height.

---

Rules digest: `RULES.md` §"Generative UI". Reference: `/agent`,
`/op-components#genui-ledger`. Primitives: `web/packages/op/src/agent.tsx`.
