# Temps localisation

Temps ships in English. This document is about what has to be true *now* so
that shipping a second locale later is a translation job and not a redesign.
Every rule here costs nothing today.

Companion to `content.md` (the words) and `design-system-handoff.md` §8 (the
data rules). Formatting is enforced by `@temps-sdk/op` → `src/fmt.ts`.

---

## 1. Text expansion

German runs about 30% longer than English; Finnish and Russian more. Assume
it and stop designing to the width of the English word.

- Design every label at 130% of its English length, and every button at 200%.
  `Save` is four characters and `Änderungen speichern` is twenty-one.
- Never give a text container a fixed width. Frames have edges; text inside
  them wraps or truncates.
- Never set a `w-[7rem]` on a button, a tab, a badge or a table header to make
  a row line up. Line rows up with a grid track that can grow.
- Truncate with `truncate` plus a `title` carrying the full string, never with
  a hand-cut substring and `…`.
- Let an action bar scroll sideways (`ScrollRow` / `.op-scroll-x`) rather than
  wrap into a second row when the verbs get longer.
- Keep the state vocabulary to one word (`live`, `failed`, `degraded`) so a
  glyph plus a word still fits a cell in any language.
- Test the longest string you can imagine at 390px before you call a screen
  done; the German build is not the place to find out.

## 2. No sentences built from fragments

- Never concatenate a sentence. `n + ' ' + noun`, `verb + ' ' + object` and
  `'Deployed ' + when` are all broken in any language with cases or a
  different word order.
- Write one template per sentence with named slots, and translate the whole
  template: `Serving {environment} since {time}.`
- Keep the slots named, never positional: a translator has to be able to move
  `{time}` in front of `{environment}`.
- Never split a sentence across elements to style the middle of it; wrap the
  part you are styling inside the template instead.
- Never translate a fragment that appears in more than one sentence: the same
  English word is two different words elsewhere.
- Write plurals with `fmtCount`, never with a trailing `s` or `(s)`.
- Write ordinals, lists and units through `Intl` (`Intl.PluralRules`,
  `Intl.ListFormat`, `Intl.NumberFormat`), never by hand.

## 3. Numbers, dates, currency

- Format every number, percentage, size, duration and date through `fmt.ts`,
  which takes a `locale` and defaults to the runtime's.
- Read the operator's locale from one place (the account setting, falling back
  to the browser), and pass it down; never call `toLocaleString()` with no
  argument in one component and `'en'` in the next.
- Never assume the decimal separator, the group separator, the digit shape or
  the first day of the week.
- Never build a date from `${d.getDate()}/${d.getMonth() + 1}`. `Sep 6` and
  `6 Sep` and `9/6` are all correct somewhere.
- Keep numbers tabular and mono in every locale; that is a typographic rule,
  not an English one.
- Keep the time zone policy of `content.md` §6: the reader's own zone by
  default, a named zone whenever the value is quoted or exported.

## 4. RTL readiness

Arabic and Hebrew are not on the roadmap, but the cost of being ready is one
class name.

- Use logical properties everywhere: `ps-*` / `pe-*`, `ms-*` / `me-*`,
  `text-start` / `text-end`, `start-0` / `end-0`, `border-s` / `border-e`.
  Never `pl-*`, `pr-*`, `left-*`, `right-*`, `text-left` in a layout.
- Set `dir` once, on the document, and let the cascade do the rest; never
  read `dir` in a component to pick a class.
- Flip icons that mean direction: chevrons, arrows, back and forward,
  next/prev in a pager, the `→` between two nodes in a `Flow`.
- Do not flip icons that mean a thing: a clock, a logo, a git provider mark,
  a play button on a recording, a checkmark, a state glyph.
- Keep charts LTR: a time axis runs left to right in every locale, because
  the deploy markers, the sparkline and the waterfall are read against the
  same axis everybody else quotes.
- Keep code, logs, ids, paths, commands and stack traces LTR and mono, in
  their own `dir="ltr"` container, so a mixed line cannot reorder itself.
- Mirror the shell (sidebar, breadcrumb, action bar) and nothing inside a
  `LogViewer`, a `Waterfall`, a `StackTrace` or a diff.

## 5. What is deliberately not localised in v1

Translate the console's own voice. Do not translate the machine's.

| Not localised | Why |
|---|---|
| Log lines | They are quoted from another system. A translated log cannot be pasted into a search or an issue. |
| Error output from a build, a database, a proxy | The operator will search that exact string. Translating it deletes the only lead. |
| Ids, tags, digests, branch names, hostnames, paths, header names | They are identifiers, not words. |
| Commit messages and PR titles | They belong to the repository, not to us. |
| Code, CLI commands, config keys, env var names, SQL | They are typed back verbatim. |
| Status vocabulary in the API (`ok`, `warn`, `error`, `idle`, `sampled`) | The wire value is a key; the *label* on screen is translated, the key is not. |
| Product and provider names | Temps, GitHub, PostgreSQL are spelled one way everywhere. |

- Say a machine string is a machine string by setting it in mono; that is how
  the reader knows nobody edited it.
- Localise the sentence *around* the quote, never the quote.

## 6. Before you ship a locale

- No string is built by concatenation; every sentence is one template.
- Every count goes through `fmtCount`; no `(s)`, no manual `s`.
- Every number and date goes through `fmt.ts` with the operator's locale.
- No fixed-width text container; every truncation has a `title`.
- Every label and button still fits at 130% and 200% length, at 390px and
  1440px, in light and dark.
- No `pl-`/`pr-`/`left-`/`right-`/`text-left` in a layout; logical properties
  only.
- Direction icons flip, thing icons do not; charts, logs and code stay LTR.
- Machine output is untranslated and in mono; the sentence around it is
  translated.
- Screen-read one page in the new locale: the reading order still matches the
  visual order.
