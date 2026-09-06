# Temps content: the words

Companion to `brand-guidelines.md` (how a surface looks) and
`design-system-handoff.md` (what it is made of). This one fixes the words:
one term per concept, one shape for an error, one way to write a number.

The voice is already set by the console: short sentences, imperative, no
adjectives the reader cannot verify. A fact appears once. The reader is an
operator at 02:00 with nobody to ask.

Rules are imperative and one line. Enforcement for the number, date and
duration rules lives in `@temps-sdk/op` → `src/fmt.ts`.

---

## 1. Capitalisation

- Write everything in sentence case: page titles, section titles, buttons,
  tabs, menu items, table headers, dialog titles, toasts.
  - Good: `Roll back to dep_90e` · `Deploy hooks` · `type the name to confirm`
  - Bad: `Roll Back To dep_90e` · `Deploy Hooks`
- Spell product names the way their owners spell them: Temps, Temps Cloud,
  GitHub, GitLab, Bitbucket, Cloudflare, PostgreSQL, MariaDB, MongoDB,
  ClickHouse, Docker, Hetzner, WireGuard, OpenTelemetry, S3.
- Never re-case an identifier, a value, or anything a machine produced:
  `dep_91a`, `main`, `sha256:9e21c7`, `CVE-2025-30204`, `p95`.
  - Good: `Failed at build container image` · Bad: `Failed At Build Container Image`
- Set every id, path, host, branch, header, env key, CLI command and quoted
  log line in mono. Prose about them stays in the sans face.
- Get uppercase from `.op-label`, never from the keyboard: type `error rate`,
  let the class shout it.
- Start a sentence with an identifier only when it is in mono; otherwise
  rewrite so the sentence starts with a word.
- Capitalise a state word only when it starts a sentence: the vocabulary is
  `live`, `failed`, `building`, `degraded`, not `Live`, `Failed`.
- Keep acronyms as they are (TLS, DNS, CPU, RAM, SDK, CLI, PITR, SSO) and
  expand them once, on the surface where they first appear.

## 2. Terminology

One term per concept. A synonym is a second concept the reader has to rule
out. The banned column is not a style preference: it is a promise that if
the word changed, the thing changed.

| Use | Means | Never |
|---|---|---|
| **deployment** (noun) | one record of one build and release, identified by a tag (`dep_91a`) | release, build (as the record), version, push |
| **deploy** (verb) | start a deployment | ship, publish, push (as the verb), release |
| **roll back** (verb) / **rollback** (noun) | put an earlier deployment back in front of traffic | revert (that is git), undeploy, downgrade |
| **redeploy** | run the same commit again | rebuild, retry the deploy |
| **project** | the unit that owns a repository, environments, domains and deployments | app, application, site, service, workload |
| **kind** | what a project is: app · worker · static | type, category, flavour |
| **service** | one traced upstream inside a trace (`stripe`, `postgres`) | anything a project is |
| **environment** | production · staging · preview | stage, tier, env (in prose; `env` is fine as a mono key) |
| **domain** | a hostname pointed at a project | URL, site, address, endpoint |
| **node** | one machine in the cluster, with a role word: control plane · worker | server, host, instance, box, VM, machine |
| **worker** | a node whose role is running workloads, or a project of kind worker — say which | agent, runner, slave |
| **provider** | a connected external account: git provider, storage provider, DNS provider | integration, connector, connection, plugin, app |
| **backup** | a copy of a database, taken on a schedule or on demand | snapshot, dump, archive, export |
| **restore** | put a backup back | recover, import, roll back a database |
| **variable** | one row of configuration on an environment; it is plain or secret | env var, config, setting, key/value |
| **secret** | a variable whose value is hidden until revealed | credential, password (unless it is one), token (unless it is one) |
| **issue** | a group of identical errors | bug, exception, problem, alert |
| **event** | one occurrence: one error, one analytics hit, one email | hit, record, entry, log |
| **monitor** | one uptime target; a **check** is one probe of it | ping, healthcheck (as the noun for the target) |
| **run** | the record of one agent execution | session, job, task, conversation |
| **proposal** | a finding turned into a scoped change with evidence, impact, risk and a verification plan | suggestion, recommendation, insight |
| **autonomy level** | observe · propose · act with approval · autopilot | mode, permission level, trust level |
| **sign in** / **sign out** | the human entering or leaving the console | log in, login (as a verb), log out, authenticate |
| **member** | a user inside a team; **user** is the person and their account | seat, teammate, collaborator (unless the git provider's own word) |
| **remove** | detach a thing from another thing; the thing survives | delete (when nothing is destroyed), unlink, disconnect |
| **delete** | destroy data irreversibly | remove (when data dies), purge, wipe, drop |
| **cancel** | stop something queued or in progress before it finishes | abort, kill, terminate |
| **stop** | halt something running that will start again | shut down, kill, pause |
| **retention** | how far back the data goes on this plan | history, window (that is the range you picked) |
| **sampled** | kept only up to the plan's allowance | throttled, rate-limited, dropped |

- Say `remove` or `delete` by what happens, not by how the button feels:
  removing a domain from a project is `remove`; deleting the project is
  `delete`, and only `delete` goes red.
- Name the record's own id the way the API does (`dep_91a`, `i_4821`,
  `scan_9a1`), and never invent a display id.
- Write the AI vocabulary exactly as `brand-guidelines.md` §0 fixes it: agent,
  skill, tool, MCP server, proposal, approval, run, autonomy level.
- Add a term to this table before you use it on a screen; two words for one
  thing is a bug report waiting to be filed.

## 3. Error messages

Shape: **what failed · on what · why · what to do next** — with the
identifier, always.

- Name the operation that failed in the first three words.
- Name the resource by its real name and its id.
- Quote the other system verbatim in mono; never paraphrase a machine.
- End with the next action, and make it a control the reader can see.
- Never write "something went wrong", "an error occurred", "unexpected
  error", "failed to complete", "please try again later" or "contact
  support": a self-hosted operator has nobody to contact.
  - Good: `Build failed at build container image on api-gateway dep_92e after 12s: next build exited 1 — type error in src/checkout/AddressForm.tsx:88. Nothing changed in staging. Open the build log.`
  - Bad: `Something went wrong while deploying. Please try again.`
  - Good: `Backup db_orders_nightly failed at upload: the S3 endpoint returned 403 AccessDenied. The dump is kept for 24h. Check the storage provider's credentials.`
  - Bad: `Backup error (403).`
- Say what did not change, when nothing did: `Staging stayed on dep_89f.`
- Give a `PageState state="error"` the message, the resource and a retry —
  all three, or it is not finished.
- Give a `Callout` the raw quote and one sentence of consequence; put the
  fix in the action, not in the sentence.
- Write the fix as a thing to do, not as a possibility: `Add a DNS TXT record
  for _acme-challenge.api.acme.sh.`, not `The DNS record may be missing.`
- Never blame the reader (`invalid input`); say what the field takes:
  `Names take lowercase letters, digits and dashes.`
- Never expose a stack trace as the message; the message is one sentence and
  the trace is `StackTrace` under it.

## 4. Empty, loading, unconfigured, confirmation

- Write an empty state as fact then next step: `No deployments yet.` +
  `Connect a repository to deploy on push.` Never `Nothing here!`.
- Say why it is empty when there is more than one reason a chart can be
  blank: no traffic · not configured · sampled · past retention.
- Write an unconfigured state as three things: what is missing, one example
  of what the surface would show, and the link that fixes it.
  - Good: `Missing: an S3 bucket and credentials.` + `Configure storage`
  - Bad: hiding the page, or `Coming soon`.
- Never write copy for a loading state: `PageState state="loading"` is
  skeleton rows. No "Loading…", no "Please wait", no spinner sentence.
- Write a confirmation title as the action itself (`Delete api-gateway`), the
  description as what is lost and what is kept, and the button as the title
  in lower case — `EchoDialog` does the rest.
  - Good: title `Delete database orders`; description `Deletes the volume and
    all 14 backups. The last nightly dump is not kept. This cannot be undone.`
  - Bad: title `Are you sure?`; description `This action is permanent.`
- Say how to undo it, when it can be undone: `Rolling back to dep_90e takes
  about 5s.` Then it is not red.
- Confirm in the past tense, once, where the action was: `copied`,
  `rolled back to dep_90e`, `couldn't copy · press ⌘C`.

## 5. Buttons and actions

- Verb first, object second: `Roll back`, `Add domain`, `Rotate key`,
  `Open build log`. Never a bare noun (`Domains`) on a button.
- Drop the object when the row already names it: a row for `api.acme.sh` gets
  `Remove`, not `Remove api.acme.sh`.
- Never label a button `OK`, `Yes`, `No`, `Submit`, `Confirm` or `Done`; the
  button says what it does, and its twin says `cancel`.
- Name the loss in a destructive button: `Delete project and 14 backups`.
- Keep an action to three words; if it needs more, the dialog carries the
  rest.
- Never label something its position already explains (`Queued` above a queue).
- Write a link as where it goes (`Open the issue`), never `click here`,
  `learn more` or `read the docs`.
- Use the same verb for the same operation everywhere: one `Roll back` in the
  ledger, the detail, the palette and the toast.
- Write a menu item as a sentence fragment, no trailing period.

## 6. Numbers, units, dates, durations, time zones

These are `src/fmt.ts`. Use the function; do not hand-roll the string.

- Set every number in mono and tabular, with the unit after it in muted ink
  (`Num`). `184ms`, not `184 ms` in a cell and `184ms` in the next.
- Group thousands with `fmtNum`, which uses the operator's locale: `30,800`
  in `en`, `30.800` in `de`. Never a hand-rolled regex, never a hard comma.
- Write a percentage with one decimal (`fmtPct`): `0.6%`. Drop to zero
  decimals only inside a breakdown where every row is a whole share.
  - Good: `error rate 0.61% since dep_91a` · Bad: `error rate <1%`
- Never show a delta without its baseline: `+0.49pt since dep_91a`, not `+9%`.
- Write sizes in decimal bytes by default (`fmtBytes`): `212 MB`, `18.8 MB`,
  `1,204 files`. Decimal, because the reader compares the number against a
  bandwidth bill and a provider console, and both are decimal.
- Write memory and page cache in binary units and say so in the unit
  (`fmtBytes(n, { binary: true })` → `1.5 KiB`, `2 GiB`): that is what the
  kernel reports, and rounding it to `MB` invents a discrepancy.
- Write a duration in at most two units (`fmtDuration`): `812ms`, `9.4s`,
  `41m 12s`, `2h 05m`, `3d 4h`. Never `145s` for a build that took `2m 25s`.
- Write latency at the precision it is measured: `184ms`, `0.81s`, `812µs`.
- Write time relative under 24 hours and absolute after (`fmtRelative`):
  `41m ago`, `10h ago`, then `Sep 6 at 20:33`. A reader cannot subtract
  "9 days ago" from today.
  - Good: `20:33 today · dep_91a` · Bad: `just now` · Bad: `2026-09-06T20:33:00.000Z` in a row
- Put the id beside every time: a time answers "when", the id answers "which
  one", and an operator needs both to say a sentence out loud.
- Give every relative time a `title` with the absolute stamp
  (`fmtAbsolute`), so the exact moment is one hover away and never a click.
- Render absolute times in the reader's own time zone, unnamed — that is the
  clock on their wall during the incident.
- Name the zone whenever the time will be quoted, exported or compared
  across people: `fmtAbsolute(t, { tz: 'UTC' })` → `Sep 6 at 20:33 UTC`.
- Never convert a machine's own timestamp: a log line, a stack trace and a
  quoted error keep the clock they were written with, verbatim.
- Write a count with its noun through `fmtCount`, never by adding an `s`:
  `1 deploy`, `6 deploys`, `0 issues`.
- Write nothing as an en dash (`EMPTY`, `–`) and zero as `0`. "We have no
  number" and "the number is zero" are different facts.
- Never round a number into meaninglessness: `0.6%` not `<1%`, `30.8k` only
  where the exact count is in the tooltip.
- Write a range with an en dash and no spaces: `1–20 of 1,284`,
  `0.5–2 cores`.
- Write a version, a commit and a digest short and mono: `9bc61c0`,
  `sha256:9e21c7`, `v0.1.2`.

## 7. Punctuation

- Separate facts on one line with a middle dot, spaced: `worker · production
  · 2 replicas`. Not a pipe, not a bullet, not a slash, not a comma.
- Use a slash only inside a path or a breadcrumb (`platform / projects /
  api-gateway`).
- End a sentence with a period. End a label, a cell, a column header, a
  button, a tab or a menu item with nothing.
- Never use an exclamation mark. Nothing in an operator console is exciting,
  and the one thing that is (an outage) does not need one.
- Never ask a question the reader cannot answer: no `Are you sure?`, no
  `Something not right?`.
- Use an ellipsis character (`…`) only for truncation, and only where the
  full value is on hover or one click away. Never for suspense
  (`Deploying…` is a state word plus a glyph, not a sentence).
- Use an em dash sparingly for a consequence (`exited 1 — type error in
  AddressForm.tsx:88`); use an en dash for ranges and for nothing.
- Quote a machine's words in mono, not in quotation marks.
- Write a possessive on a mono id with a plain apostrophe outside the mono
  span, or rewrite the sentence.
- Use `·` between a key and its badge, never a colon, in a footer or a meta
  line: the colon belongs in `KeyValue`.
- Use straight quotes in code and typographic quotes in prose, and no quotes
  at all around a state word.

## 8. Before you ship

- Read every string on the screen out loud; anything you would not say to a
  colleague at 02:00 goes.
- Search the diff for `toFixed`, `toLocaleString`, `+ 's'` and a hard-coded
  `,` in a number: each one is a rule not being enforced.
- Search for a banned synonym from §2 in the strings you added.
- Check that every error you can trigger names its resource and its next
  action.
- Check that the same operation reads with the same verb in the ledger, the
  detail, the palette and the toast.
