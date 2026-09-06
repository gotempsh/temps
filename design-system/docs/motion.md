# Motion

Restraint is in colour, not in movement — but a console is read, not watched.
Motion here has one job: tell the reader that *they* caused something, or that
a value they are watching just changed. It never introduces anything, never
decorates, and never asks to be waited for.

The tokens are in `web/packages/op/tokens.json` (`base.duration.*`,
`base.easing.standard`) and declared on `.operator.ink` in `op.css`.
`node web/packages/op/scripts/tokens.mjs check` fails if the two disagree.

## The tokens

| Token | Value | Use |
|---|---|---|
| `--op-duration-fast` | `80ms` | A control acknowledging the pointer: an inline action icon coming up out of muted, a hover tint on a tile. Below 80ms the change reads as a flicker rather than a response. |
| `--op-duration` | `100ms` | The default, and a frozen decision (handoff §4, "Motion is 100ms"). Every state change of a control: fill, border, transform, shadow, colour. |
| `--op-duration-slow` | `200ms` | The ceiling, and only for something arriving *on top of* the page: a dialog, a drop, a toast. Nothing in the page body may take this long. |
| `--op-ease` | `cubic-bezier(0.2, 0, 0, 1)` | The one curve. Fast out, settled end. A second curve is a second opinion about the same 100ms. |

There is no fourth duration and no second curve. If something needs 400ms it is
not a state change, it is an animation, and it does not belong on the screen.

## What may move

- **A control changing state.** A button filling on hover, a switch track
  inverting and its thumb translating, a tab taking the underline, a row
  taking the accent surface under the cursor. `--op-duration`.
- **A drop opening.** Popover, picker, command palette, dialog: it fades and
  scales the last 5% into place so the reader sees *where it came from*.
  `--op-duration-slow`.
- **A row entering focus.** `j`/`k` moves DOM focus; the surface follows at
  `--op-duration` so the eye can track the jump. The focus ring itself is
  instant — a focus ring that fades in is a focus ring you can miss.
- **A live value updating.** A number that just changed may flash its cell
  background once at `--op-duration`. The digits themselves never slide, count
  up, or roll: a rolling number cannot be read while it rolls.
- **An inline action icon on hover.** Muted → foreground at
  `--op-duration-fast` (brand §5: inline action icons are 14px, muted until
  hover).

## What never moves

- **Layout.** No height animation, no reflow transition, no accordion slide.
  A section that grows moves everything below it, and the reader loses their
  place. `.op-block`, `.op-halves`, `.op-kv` and the ledger have no transitions
  at all — this is why the blanket rule lists only `transform`, `box-shadow`,
  `background-color` and `color`, and deliberately omits `height`, `width`,
  `margin`, `padding` and `opacity`.
- **Page transitions.** Navigation is instant. A fade between routes is 200ms
  of the reader staring at nothing on every click.
- **Charts drawing themselves.** Lines appear complete. An animated line is
  unreadable for the whole time it is animating, and it lies about when the
  data arrived. (Handoff §4, frozen: "Charts are linear lines, ink on paper,
  no fills, no animation.")
- **Skeletons shimmering.** `PageState state="loading"` renders static
  skeleton rows in the shape of the content. A shimmer is decoration on top of
  an absence.
- **Anything decorative.** No parallax, no reveal-on-scroll, no hover lift on
  something that is not a control, no confetti, no gradient sweep, no marquee.
- **The `.op-raise` offset.** See exception 1.

## Reduced motion

`@media (prefers-reduced-motion: reduce)` in `op.css` sets all three duration
tokens to `0s` and forces `transition-duration`, `animation-duration` and
`animation-iteration-count` on every descendant of `.operator.ink`. One rule,
one place. Every element still arrives at exactly the same end state, so
nothing is lost — including the two exceptions below, which stop as well: a
skeleton is still a skeleton and the retry button still reads "retrying…"
without their animation. A reader who asked for no motion asked for no motion.

Do not gate motion on `prefers-reduced-motion` in JavaScript. The one existing
JS check (`design-system/src/components/system-map-section.tsx:312`) guards a
sandbox-only demo animation and is not a pattern to copy.

## The two exceptions that exist today

**1. The `.op-raise` shadow is a hard 3px offset that does not move.**
`--op-raise-shadow: 3px 3px 0 0 var(--foreground)` is a printed offset, not a
depth cue: no blur, no spread, and — unlike every other raised UI convention —
it does not lift on hover or press. `.op-raise` is the one raised element per
screen and it is raised *permanently*, because it marks the thing the reader
must act on, not the thing the pointer happens to be over. Only
`button.op-primary` moves into its shadow, and only on `:active`
(`translate(1px, 1px)`, shadow 2px → 1px): that is the press, and a press has
to be felt. The earlier `.hardline` skin animated a hover lift on `.op-raise`;
ink deliberately dropped it.

**2. Two animations survive the "no motion" rule: `animate-pulse` and
`animate-spin`.** The blanket transition rule excludes them by selector
(`*:not(.animate-pulse):not(.animate-spin)`) because both communicate "still
working", which an instant state change cannot express at all. They are used
in exactly two places in the package: `Skeleton` (`ui/skeleton.tsx`, the
`PageState` loading rows) and the retry button's `RefreshCw`
(`page-state.tsx:80`). Anywhere else, a spinner is banned — `Loader2` as a
page state is in the RULES ban list, and a shimmer on a skeleton is not one of
these two.

A third animation exists and is a deliberate borrowing rather than an
exception: `.op-caret::after` (`op-blink`, `1s steps(1) infinite`) is the
streaming caret on agent text and tool input. It is a *terminal* caret drawn as
text, it steps rather than fades, and it stops under reduced motion like
everything else. Do not use it anywhere a terminal is not being imitated.

Two things that look like they should move and deliberately do not, both
verified in code: the `Live` indicator (`viz.tsx:486`) is a static `●` plus the
word "live" — it does not pulse, because a pulsing dot is a decoration that
says nothing a glyph and a word do not — and a `Num` that changes value
re-renders without a transition.

## Applying a different tier

The blanket rule covers the skin. To opt one element out, use the utilities
rather than a literal:

```tsx
<button className="op-motion op-motion-fast opacity-60 hover:opacity-100">
```

- `.op-motion` — adds `border-color` and `opacity` to the transitioned
  properties, on `--op-ease`.
- `.op-motion-fast` / `.op-motion-slow` — swap the tier.

Never write `duration-150`, `transition-all`, or a literal `ms` in a component.
`transition-[color]`-style utilities that only name a *property* are fine; the
duration comes from the token.

## Known gaps

- The sandbox's own `globals.css` still hand-rolls `.fade-in-0`, `.zoom-in-95`
  and friends at literal `150ms`, and `system-map-section.tsx` uses
  `duration-300`. Those are sandbox call sites, outside `@temps-sdk/op`, and
  are listed for the coordinator rather than changed here.
- The dialog primitives now carry
  `[transition-duration:var(--op-duration-slow)]` instead of `duration-200`,
  but their *entrance* timing still comes from the sandbox keyframe utilities
  above, not from the token. Closing that needs the sandbox pass.

---

Rules digest: `RULES.md` §Motion. Tokens: `web/packages/op/tokens.json`.
Reference: `/op-components`, `/v1`.
