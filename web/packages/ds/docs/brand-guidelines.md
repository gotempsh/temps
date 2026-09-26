# Temps console — brand guidelines

## Why: the emotional job

The person looking at this screen is a developer or operator, usually
mid-task — configuring a service, watching a deploy, triaging an alert at an
hour they'd rather not be awake for. They did not come here to be delighted.
They came here to find out, in the next three seconds, whether something is
broken and what to do about it.

Every design decision in this system optimizes for that: clarity and speed
over decoration, honesty over polish. A console that looks impressive but
buries the one number that matters has failed at its actual job. This is the
first of the six interview answers this package is built from (see
`web/docs/design/decisions.md`) and it governs everything below.

## The signature

Restrained monochrome, one state accent. Near-black `--primary`, white/gray
surfaces (`--background`, `--card`, `--muted`), state color reserved for
`--success`/`--warning`/`--destructive`. This isn't a new look invented for
this package — it's what `web/src/globals.css` already does, and the
signature is "don't dilute it," not "introduce it."

This directly extends the standing brand rule (`--temps-brand-colors-white-black`
in project memory: white/black, not blue) and rejects the earlier "operator
ink" redesign's instinct to invent a new visual skin. Codification, not
redesign — see the retirement note at the top of `docs/design-system-handoff.md`.

## Colour policy: state and provider identity

Colour is not a decoration budget. A blue button, a green sidebar icon, a
purple chart line chosen because it "looks nice" is a bug in this system —
every one of the five `Status` tones (`ok`/`warn`/`error`/`idle`/`running`)
maps to a specific meaning, and interface controls should not borrow those hues for decoration. Charts get color per-series only when the series
identity itself needs distinguishing (`--chart-1..5`), never to decorate a
single-series panel.

Small provider logos are the identity exception: reuse the existing GitHub,
GitLab, Bitbucket, and Gitea artwork through `GitProviderMark`. Preserve brand
colors inside the mark only; cards, labels, selection borders, and buttons
remain neutral. GitHub adapts to light/dark surfaces. A green Gitea mark is
not a successful connection — show connection health separately with `Status`.

## Taste: what "good" looks like here

- A record page answers "is this OK?" before the user finishes scrolling to
  the fold — that's the record recipe's verdict beat, not an afterthought.
- Empty is not the same as broken, and broken is not the same as "you forgot
  to set this up." Three different `PageState` variants exist because
  conflating them produces support tickets from users who can't tell which
  one they're looking at.
- A form that goes wrong should say which field and why, in the field's own
  space (`Field`'s error slot) — not just at the top, not just in a toast the
  user has already dismissed by the time they scroll down.

## Anti-patterns

- **Decorative color.** A status color used because a screen "felt gray."
- **A second visual language.** Anything resembling the old op package's
  glyph vocabulary (`●◐×○◉`) or an "ink" skin class. That effort was
  cancelled specifically because it invented a new look instead of
  formalizing the one already shipping — see the retirement note.
- **Silence as a state.** A feature with no operator config rendering an
  empty screen instead of a `PageState` `not-set-up` surface. This is a
  CLAUDE.md violation, not a style nit.
- **A toast for something the user is already looking at.** If the user's
  cursor is still on the button, the answer belongs on the button
  (`CopyAction`), not in a corner notification they might not see.
- **Inventing a new component for a solved problem.** A new stat-tile
  component instead of extending `TimeChart`; a new page-header row instead
  of `PageHeader`'s `actions` slot. Check the primitive catalogue in
  `design-system-handoff.md` before writing a new one.
