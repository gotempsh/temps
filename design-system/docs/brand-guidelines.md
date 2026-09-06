# Temps brand and hierarchy guidelines

Companion to `operator-console-brief.md` (unchanged). That document describes the
console. This one fixes the rules that make a Temps surface look designed rather
than generated, and it applies to the landing page, the docs, and the console alike.

## 0. What Temps is: an AI-native platform under your policy

The roadmap (temps.sh/roadmap, "The road to self-driving products") fixes the
positioning: Temps is a self-hosted platform that first replaces the SaaS
stack, then runs an improvement loop over it: sense, understand, decide, act,
learn. Autonomy is earned one release at a time: stabilise (v0.1), observe
(v0.2), propose (v0.3), improve on autopilot (v1.0), and every level stays
bounded by environments, permissions, budgets and blast radius. Agents,
skills, MCP servers and scheduled automation are not features bolted onto a
PaaS; they are how the product works. The brand has to say so without a
single sparkle.

Design consequences, in order of importance:

1. AI is an operator, so it is held to operator rules. An agent's work is a
   ledger of typed tool calls with state words, not a chat bubble. Its
   findings carry evidence, confidence, impact and a verification plan, or
   they are not shown. Its writes are proposed, previewed with redacted
   parameters, and confirmed. The agent conversation (`/agent`) is the
   reference surface.
2. Autonomy is a control, not a mood. Every capability shows which level it
   runs at in words: observe · propose · act with approval · autopilot. The
   level is set per capability, has a budget, an override and a kill switch,
   and the console always shows what ran, why, and how to undo it.
3. Evidence before adjectives. "Found in 31 events since dep_91a, confidence
   high, verified by the checkout suite" is brand copy. "AI-powered insights"
   is not. No sparkles, no gradients, no magic-wand icons; the kind icons are
   a bot, a terminal, a file, a checklist.
4. Extensible in the open. Skills, MCP servers and tools are first-class
   nouns with their own ledgers (name, source, permissions, last run). They
   are configured like a git provider is configured, and unconfigured ones
   onboard instead of disappearing.
5. The human is the governor, and the copy says so: "Temps prepares the
   change. You review and approve it." Never "let AI handle it".

Vocabulary, fixed: agent (a bounded run with a goal), skill (a reusable
instruction set an agent can load), tool (one typed call), MCP server (a
source of tools), proposal (a finding turned into a scoped change with
evidence, impact, risk and a verification plan), approval (the human gate,
inline, never a modal), run (the record of what happened), autonomy level
(observe / propose / act with approval / autopilot). Use these words and no
synonyms.

## 1. The direction: paper and ink

- Paper and ink only. Backgrounds are warm off-white (`oklch(0.975 0.004 95)`),
  text is near-black. Dark mode inverts the same pair.
- Every border is ink. No grey hairlines except row dividers inside a ledger,
  which use `--op-rule-soft` (16% ink).
- No cards. One raised element per screen (`.op-raise`), which is the thing the
  reader is supposed to act on.
- Colour means status. Green, amber, red appear only next to a glyph and a word.
- Dense by default. Whitespace is spent between sections, not inside tables.

## 2. Hierarchy

The problem the landing had: everything was weight 500. Eyebrow, headline,
tool strip, lead and buttons all competed, so nothing led. The fix is a fixed
scale where weight alone tells the reader what tier they are on.

| Class         | Weight | Tracking | Size                          | Use                                  |
|---------------|--------|----------|-------------------------------|--------------------------------------|
| `.op-display` | 800    | -0.04em  | clamp(2.75rem, 8vw, 6rem)     | Hero headline. One per page. Never in the console. |
| `.op-h1`      | 700    | -0.03em  | clamp(2rem, 4.2vw, 3.25rem)   | Major section title.                 |
| `.op-h2`      | 600    | -0.02em  | clamp(1.375rem, 2.4vw, 1.75rem) | Minor section or panel title. The console's largest tier. |
| `.op-h3`      | 600    | -0.01em  | 1rem                          | Item title inside a grid or row.     |
| `.op-lead`    | 400    | normal   | clamp(1.0625rem, 1.5vw, 1.25rem) | The sentence under a title. Muted. `<strong>` for one phrase. |
| body          | 400    | normal   | 0.875–0.9375rem               | Everything else.                     |
| `.op-label`   | 500    | +0.1em   | 10–11px uppercase             | Eyebrow, column header, key badge.   |

Rules:

1. One display headline per page. If two things are the biggest, neither is.
2. Titles are 700 or 800. Body is 400. Labels are 500. There is no 500 title.
3. A title's lead is muted. Bold inside a lead is reserved for the one phrase
   that changes the reader's mind.
4. Numbers that matter are mono, tabular, and one tier larger than their label.

## 3. Section rhythm

Sections come in two tiers and two tones. Alternate them.

| Attribute            | Effect                                                  |
|----------------------|---------------------------------------------------------|
| `data-tier="major"`  | `.op-h1` title, optional lead, 5rem vertical padding.   |
| `data-tier="minor"`  | `.op-h2` title, no lead, 3.5rem padding.                |
| `data-tone="muted"`  | Background switches to `--muted`. The only allowed change. |
| `.op-fill`           | Filled with `--primary`. Once per page, for the closing CTA. |

Never stack three major sections. Never put two muted sections back to back.
The hero has no border above it and no tone; it is the only section that is
centred.

Inside a console page the rhythm is fixed too, so two pages built by two
people look like one application:

- The spacing scale is 4 · 8 · 12 · 16 · 20 · 24 · 32 px and nothing else.
- Five type tiers, each at its own size, so the page can be ranked before it
  is read: title 700/20 · lede 600/18 with its glyph · section title 600/14 ·
  the event or state word in a row 500 · everything else 400 muted. Nothing
  else is bold. Two things at the same size and weight are peers.
- The lede is the one raised element on a record page (`.op-raise`), directly
  under the title: the record's state as glyph + one word, then one muted
  sentence with the fact that matters. It is the shape the eye lands on
  first; everything below it is detail. The header's attention count is the
  shell's copy of the same verdict; the lede is the page's.
- Below the lede the page is a main column and, at xl, a narrow aside
  (18rem) on the right. The main column holds the thing itself first
  (content, with a 2-view Segmented when it has two faithful renderings such
  as html/text), then what happened to it (Events). The aside holds
  reference facts (headers, identifiers) as a compact key-over-value list at
  11px. Below xl the aside stacks under the main column behind an ink rule.
  Every block spans the page; see §6 on width.
- Grouped lists (KeyValue, Timeline) are framed: an ink border around the
  group, soft rules between rows. The frame is what turns loose lines into a
  thing the eye can find.
- An event is drawn by an icon that says what kind of event it was (queued,
  sent, delivered, opened, bounced), never by a coloured dot. A dot only says
  fine/not fine; the icon says what. Colour on the icon is reserved for
  failure (red) and not-real (muted). One icon per event kind, used the same
  way on every page that shows that event.
- A fact appears once. If it is in the title's meta or the lede, it is not
  repeated as the first row of a section.
- A section is one title (600, 14px) with its count or one fact in mono
  beside it, 12px, then exactly one body: a framed list of facts, a framed
  timeline, a ledger, a chart, a form, or content. Never two bodies under one
  title, never a body without a title. Sections in one column are separated
  by an ink rule with 20px above and below; the first has none.
- Ink rules separate sections, soft rules separate rows, frames enclose
  groups and content, the one raise marks the lede. Nothing else draws a line.

This is the grouped-list discipline of a system settings screen: the reader
finds the section by its title, the fact by its key, the state by its glyph,
and every page teaches the next one.

## 4. Colour and the accent

Default primary is ink. An accent, when used, is one hue applied to
`--primary` and `--primary-foreground` only. It appears on:

- the primary call to action,
- the active tab in a filled tab strip,
- the closing `.op-fill` block.

It never appears on text, borders, icons, backgrounds of sections, or charts.
At any scroll position at most one filled element should carry it. This is the
model Bun uses: one saturated hue on the install button, black everywhere else.

Available accents (`data-accent` on the `.operator.ink` root):

| Name     | Light                     | Notes                                      |
|----------|---------------------------|--------------------------------------------|
| `signal` | `oklch(0.64 0.21 32)`     | Vermilion. Highest contrast against paper. |
| `moss`   | `oklch(0.55 0.15 150)`    | Green. Reads as "healthy", which is a risk next to status green. |
| `cobalt` | `oklch(0.5 0.2 262)`      | Collides with the focus ring hue.          |
| `violet` | `oklch(0.5 0.22 300)`     | Reads as AI/marketing.                      |

Recommendation: `signal`. It is the only one that cannot be mistaken for a
status colour or the focus ring. Yellow was removed: it fails contrast on paper
and reads as a warning.

### Contrast is a token, not a review step

State colours are used as text (glyph + word, a red number, "danger zone"),
so the tokens themselves must pass AA at body size against the paper they
sit on. The operator skin therefore does not use the shadcn defaults:

| token | light (on paper L 0.975) | dark (on ink L 0.17) |
|---|---|---|
| `--destructive` | `oklch(0.53 0.21 25)` · 5.5:1 | `oklch(0.72 0.18 23)` · 7.0:1 |
| `--success` | `oklch(0.50 0.15 150)` · 5.2:1 | `oklch(0.72 0.15 150)` · 8.2:1 |
| `--warning` | `oklch(0.52 0.14 68)` · ≥4.5:1 | `oklch(0.80 0.15 75)` · 10:1 |
| `--muted-foreground` | 6.9:1 | 6.6:1 |

Text on a filled control follows: on the light red fill the text is white
(5.9:1); on the dark red fill it is ink (7.0:1), because white would be
2.7:1. The one deliberately faint colour is `--op-rule-soft` (16% ink),
which is only ever a line or an `aria-hidden` separator, never text that
carries meaning. A label or number may never be set on a coloured bar; the
number sits beside the bar in its own column.

The check is automated: the components page is audited with a contrast
script in both modes before a token or a primitive is changed, and the
audit must come back empty apart from `--op-rule-soft` separators.

## 5. Signature moves

These are what make a Temps screen recognisable at a glance. Every product
surface should use at least one.

- The verdict behind a count. Every console page has one verdict (the worst
  state on it, one sentence under 60 characters, at most one link) and it does
  not take a line of the page: it sits in the header as a glyph and a number,
  `× 2 · ◐ 1`, and the sentences open on demand when that is clicked. A page
  with nothing wrong shows one quiet green glyph and no number. Counts, facts
  and "fine" things never appear in a verdict.
  Wrong, as a line across the page: `◐ 6 projects · ✕ billing-worker failing · ◐ api-gateway 0.61% · 4 deploys today · cert 6d`.
  Right: `× 1 ◐ 1` in the header; open: `✕ billing-worker is failing health checks.` then `◐ api-gateway error rate 0.61% since dep_91a.`
- Breadcrumbs in the header, never on the page: group / list / current. The
  current crumb is the resource's real name, never its id.
- The proposal block. Every AI suggestion has the same shape, in this order:
  what (one sentence), evidence (the signals, linked), confidence (a word:
  low / medium / high, never a percentage bar), expected impact (a number
  with its baseline), risk and blast radius (environments and users touched),
  verification plan (what proves it worked), then approve · edit · deny
  inline. Missing a field, the proposal does not render; it becomes a
  finding.
- The autonomy control. A Picker that says the level in words, per
  capability, with the budget beside it (`propose · 20 runs/day · $5`). The
  same control everywhere: project settings, a scheduled agent, an MCP server.
- Deploy markers on every time axis.
- Typed confirmation on destructive dialogs: the resource name in a mono badge
  that is itself the copy button, right before the input. No command echo in the dialog;
  the CLI equivalent lives in the docs.
- Key badges on primary actions, platform-aware (⌘ on macOS, Ctrl elsewhere).
- Icons that describe the action, not the state. A control shows what it does
  (pencil edits, arrow sends, × removes, rotate retries, copy copies); a row
  shows what it is (terminal, file, agent, task). State is a glyph and a word,
  never an icon. Inline action icons are 14px, muted until hover, with a
  `title`, and sit at the end of the row they act on. Whole words are for
  results and choices ("copied", "run it", "deny"), not for repeated row
  actions.

## 6. Taste

The rules above say what a page is made of. Taste is the judgement calls
that decide whether the result looks like one considered application or a
pile of correct parts. These are the calls, written down so they are made
the same way every time.

- **Where does the eye land?** Before shipping a page, answer: first, second,
  third. First is the lede (the one raised block), second the primary
  framed group, third the aside. If two things compete for first, one is
  the wrong tier. If nothing is first, the page has no hierarchy no matter
  what the fonts say.
- **Shapes before type.** Hierarchy is made of raised, framed and loose, in
  that order, and only then of size and weight. One raise per page; frames
  around every group and every piece of content; loose only for prose and
  notes under a group. Loose rows stretched across a screen are the single
  most common way a page loses its shape.
- **Edges align.** Every block shares the page's left and right edge with the
  title row and its actions. Nothing is capped narrower than the page: the
  lede runs edge to edge, the main column grows, the aside is fixed at 18rem.
  If content is too wide to read, the fix is a measure inside the frame
  (prose at about 70ch), never a narrower frame; the frame keeps the edge.
- **Proportion carries importance.** The main column is wide because the
  thing itself matters most; the aside is narrow and 11px because it is
  reference. Two equal columns say "these are peers" and are almost always
  wrong on a record page.
- **Identity marks sit with the name.** A project's favicon or logo (and a
  git provider's mark) appears wherever the name appears and nowhere else:
  16px in a row, list or palette, 24px beside a page title, never as a tile
  or hero. It may keep its own colours; at that size it cannot compete with a
  state glyph, and a mark the reader recognises is worth more than palette
  purity. Unknown or unfetched marks are a monogram in ink, never a random
  colour, so "unknown" looks unknown.
  Where a row of marks stands without names (linked projects in a lede),
  the name shows on hover and focus with no delay: a mark is a name, and a
  700ms tooltip makes the reader doubt that.
- **Icons say what, glyphs say how.** The kind of a thing or an event is an
  icon (queued, sent, delivered; terminal, file, agent). Its state is a glyph
  and a word (● ◐ × ○ ◌). Never a coloured dot to mean an event, never an
  icon to mean a state. A row of green dots is decoration; a row of inbox,
  send, mail-check is a story.
- **Say it once.** A fact lives in one place: the title meta, the lede, or a
  row. When it would appear twice, the lower one goes.
- **Actions belong to the title.** Page actions sit right of the title on the
  same row. An action floating mid-page belongs to nothing.
  An action does something: copy, deploy, back up, resolve. A facet is
  somewhere to go, and it is already in the tab row; a button that only
  switches tabs is the same door drawn twice.
- **A drawn control is a wired control.** A filter box, a Segmented, a link
  or a button that cannot change anything is a lie the page tells with a
  straight face, and the reader spends their trust on it before they find
  out. Wire it, make it plain text, or remove it: those are the three
  endings. `href="#"` with a `preventDefault` never ships; a real
  destination is typed (`settingsHref: /${string}`) so a dead one is a
  build error rather than a click that does nothing. The same holds for a
  key: a `Kbd` badge with no handler is a drawn control too.
- **Show, do not hide.** Two faithful renderings of the same thing are a
  2-view Segmented in the section's action. Reference material gets a
  smaller place, not a closed one: no collapsed sections, no tabs, inside a
  single record.
- **One ledger per screen.** A Ledger owns the screen: its filter is `/`,
  its rows are `j` `k` `⏎`, its footer is the page's footer. Two on one
  screen means two filter boxes, two footers and two lists claiming the same
  keys. Two kinds of record are two facets (a tab each), never two tables
  stacked. When a second list truly belongs on the page, it is a Section
  with a plain framed list of at most five rows and a link to its own facet:
  no filter, no footer, no keys.
- **Icons say what kind, glyphs say what state.** A flag, a browser mark, a
  channel or device icon sits in a fixed 16px slot before the label so the
  eye can scan a list by kind without reading. State stays with the glyph
  (● ◐ × ○ ◌) and never with the icon: a red Chrome logo means nothing.
  Icons are monochrome ink at 14px, never brand colours.
- **Rank by the question, not by the header.** "Language" ranks languages
  with locales inside, because the question is "what should I serve", not
  "which Accept-Language strings arrived". A campaign is source · medium ·
  campaign as one row, with term and content as its variants, because the
  question is "did the launch work", not five separate top-ten lists.
- **A remainder is a sentence, not a bar.** Untagged visits, "unknown"
  language, "other" browsers: say the number and what it means in the hint
  ("9,038 of 12,418 visits carried no utm tags"), never a 99% bar that dwarfs
  the rows people came for.
- **Peers end on one line.** Framed lists side by side in a grid stretch to
  the tallest and pin their footer to the bottom edge, so the totals people
  compare across columns sit on one baseline. Rows stay top-aligned; a short
  list keeps honest blank space inside its frame. Never pad with fake rows
  or change `limit` to make heights match.
- **Colour only what is not fine.** In a table of measurements, a good value
  is plain ink; only "needs work" and "poor" take a tone and a glyph. A grid
  where every cell is green says nothing and the one red cell drowns. The
  same on a map: countries are filled by state, never by a ten-step gradient
  the eye cannot read back into numbers.
- **A legend does not license colour.** Colour only ever sits next to a
  glyph and a word, everywhere, with no exceptions bought back by a key.
  A legend in a hint or a footer explains a chart's lines; it does not make
  a bare red value or a bare amber word readable, because the reader who
  needs the colour is the reader who did not read the legend. If a value has
  to carry state, it carries `Status`: glyph, word, tone, in that order. A
  tone with nothing beside it is decoration at best and a wrong guess at
  worst.
- **Pick once, everything follows.** When a page has one axis of choice (a
  vital, a metric, a dimension), it is chosen once at the top and the chart,
  the map and the sort all follow it. Never one selector per panel.
- **A phone loses width, never function.** Every pattern has a phone form
  decided once, in the component, not per screen. Facets scroll sideways in
  one row with the active one in view and a fade on the clipped edge; they
  never wrap into two rows, so ten facets fit like six. Actions stay one row of
  compact buttons at their natural width and scroll the same way; they never
  stretch into full-width bars or a grid with a hole. Tiles pair two per row
  and an odd last tile spans, so the frame stays a rectangle. A record with
  more than about seven facets is over-facetted: logs, data and terminals
  are tools and belong in the actions.
- **A fault looks like a fault.** Anything that says something is broken
  carries the × in red, its title in red, and a red rule on the left, with
  no box around it: the Callout. One rule is the alert; a frame inside the
  page's frames is the third border in a row and the eye stops reading them. The other system's words are quoted in mono, never paraphrased,
  because the reader will search for them. Then one sentence of what it
  costs and what the action changes, and the action. A raised note in ink
  with a label above it reads as information, and information is not what
  a 401 is.
- **A settings row shows its value, and carries its mark.** In a settings
  hub, the row is a kind icon, the page's name and its current value in
  mono, or its problem in the state tone; never a sentence describing what
  the page is for. The icon is the same 14px monochrome mark rule as
  everywhere else (icons say what kind): "Users" with a people mark is read
  before the word is, and a column of marks lets the eye find the page
  without scanning titles. State never moves onto the icon: the glyph opens
  the value it qualifies ("× no ACME contact email"), in the value's tone.
  It does not get a column of its own, because a column of glyphs is empty
  on every row that is fine, and empty slots at the left edge push the
  substance right for nothing. The mark leads because the eye scans the
  left edge; an icon at the end of a row is decoration nobody reaches. The reader came
  to check, not to learn. And every field says when a change takes effect:
  now, next request, or restart. A setting that silently waits for a restart
  is the most common way an operator believes they fixed something.
- **Whitespace goes between groups, not inside them.** Rows are 8px, a
  title-to-body gap is 12px, the air is at the section rule (20px) and
  between columns (40px). A page that breathes inside its tables is a page
  that has to be scrolled.
- **Restraint is the accent.** Ink on paper, one raise, one glyph colour per
  state, no shadows but the hard one, no rounded corners, no gradients. When
  a screen feels flat, the answer is a better first shape, not more colour.

## 7. Do and don't

Do
- Write AI copy as an operator's log: what ran, what it found, what it wants
  to do, what proves it worked.
- Show the autonomy level in words wherever an agent can act.
- Give every page one 800-weight headline and nothing else at that size.
- Title every section inside a page at 600 with its count beside it. An
  uppercase eyebrow is a label, not a heading; a page of eyebrows has no
  hierarchy.
- Use black and white for 95% of every surface.
- Let density read as competence.
- Write errors that name the resource and the fix.

Don't
- Make a confirmation red because it needs approval. Red means irreversible
  loss and nothing else; a push, a deploy, a cache clear asks in ink and says
  how to undo.
- Use tabs on a single record. An email, a run, a finding is read top to
  bottom in one page; tabs are for a resource with facets the reader visits
  one at a time.
- Nest tabs in tabs. One row of tabs per page; a scope (environment, service,
  branch) is a Picker that reads as a sentence, "in production"; 2–4 views of
  one list are a Segmented in the toolbar.
- Put a sparkle, a gradient, a wand or the word "magic" on anything.
- Show an AI finding without evidence, confidence and a verification plan.
- Let an agent write without a proposal and an inline approval.
- Use weight 500 for a title.
- Add a second accent hue "for interest".
- Use the accent on anything with no click target.
- Ship a section with the stock shadcn card look.
- Label something that its position already explains ("Queued" over a row
  sitting between the transcript and the composer).

## 8. Where it lives

- Tokens and classes: `design-system/src/globals.css`, blocks `.operator.ink` and
  "Ink type hierarchy".
- Reference render: `/brand#hierarchy` for the scale, `/v5` for the console, `/v5-landing` for the landing.
- Landing application: `design-system/src/sections/InkLanding.tsx`, `Section`
  component (`tier`, `tone` props).
