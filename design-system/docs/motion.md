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
nothing is lost — including the exceptions below, which stop as well: a
skeleton is still a skeleton, the retry button still reads "retrying…", and the
`running` glyph is still `◉` next to the word "building". The reduced-motion
block carries its own line for the pulse, because `animation-duration: 0s` on
an `alternate` animation would otherwise leave the glyph frozen at whatever
opacity it was written with:

```css
.operator.ink .op-pulse { animation: none; opacity: 1; }
```

Full ink, held still. A reader who asked for no motion asked for no motion.

Do not gate motion on `prefers-reduced-motion` in JavaScript. The one existing
JS check (`design-system/src/components/system-map-section.tsx:312`) guards a
sandbox-only demo animation and is not a pattern to copy.

## The four exceptions that exist today

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

**2. `.op-pulse` — the one sanctioned animation in the system.** The system
does not animate; this is the single exception, and it exists because one fact
cannot be drawn still. A `running` glyph (`◉`, the sixth `State`) breathes
while work is actually happening — a build, a restore, a scan — and stops the
moment it stops. That is the whole argument: the motion *is* the fact. It is
not a loading affordance, not a decoration, and not a way to make a page look
busy; a thing that is not running must never carry it.

```css
@keyframes op-pulse { from { opacity: 1; } to { opacity: 0.45; } }
.operator.ink .op-pulse { animation: op-pulse 1.6s ease-in-out infinite alternate; }
```

Opacity only. No scale, no colour, no travel — `running` takes no tone either
(`GLYPH_CLASS.running` is plain `text-foreground`), because it is not a
verdict. 1.6s is a breath, not a tick: fast enough to read as alive, slow
enough that the eye stops going back to it. Applied by `glyphClass(state)` in
`status.tsx`, so every glyph site gets it from one place.

**It must stay in the `:not()` lists.** The blanket rules carry
`transition-duration`/`animation-duration: 0s !important`, so anything they
match cannot animate. `.op-pulse` is lifted out of all three of them —
`.operator` (`op.css:141`), `.operator.hardline` (`op.css:322`) and
`.operator.ink` (`op.css:477`) — alongside `.animate-pulse`/`.animate-spin`.
The `.op-motion` / `.op-motion-fast` / `.op-motion-slow` utilities repeat the
same `:not(.animate-spin):not(.op-pulse)` for a different reason: there it is a
specificity match, not a filter, so the utilities can out-rank the blanket
rule. **Do not "tidy" `:not(.op-pulse)` out of any of these five selectors.**
Removing it from a blanket rule kills the pulse; removing it from a
`.op-motion` selector silently breaks the motion utilities.

**3. `animate-pulse` on a skeleton.** `Skeleton` (`ui/skeleton.tsx`, the
`PageState` loading rows) is the only place it appears. A shimmer is not this;
a shimmer is decoration on top of an absence.

**4. `.op-busy` on a button doing the work.** The one spin in the system, and
the sanctioned shape the paragraph below is about. `Button` takes `busy` and
`busyLabel`: while busy it turns its own icon at `0.9s linear`, swaps the label
for the verb in progress ("saving…", "reloading…", "deploying…"), locks its
min-width to the idle width so the row it sits in does not move, sets
`aria-busy`, and swallows clicks. It is deliberately **not** `disabled` —
disabling greys the control out and, worse, drops focus, so a keyboard reader
who has just pressed ⌘S is thrown to the top of the document at exactly the
moment they are waiting to hear what happened. A minimum busy time of 400ms
(`MIN_BUSY_MS`) stops a fast answer reading as a flicker.

A reload spins because the thing it stands for goes round; a `running` glyph
pulses because a state is not an action. That is the whole difference between
exception 2 and exception 4, and it is why they do not share a class.

Under `prefers-reduced-motion` the icon holds still and the label carries it
alone — which is why `busyLabel` is not really optional. `.op-busy svg` is
lifted out of the blanket rules by `:not(.op-busy svg)` on all three
selectors, for the same reason `.op-pulse` is.

`animate-spin` is **no longer an exception.** The one legitimate spin now has
its own class (`.op-busy`, above), so nothing needs the bare utility.
Everywhere else a spinner is banned: `Loader2` as a page state is in the RULES
ban list, and the `PageState` retry button pulses instead of spinning
(`page-state.tsx`, `p.retrying && 'op-pulse'`), because a retry in flight is
the same fact as a build in flight. The selectors still name `.animate-spin`
so any remaining third-party use keeps working.

A third animation exists and is a deliberate borrowing rather than an
exception: `.op-caret::after` (`op-blink`, `1s steps(1) infinite`) is the
streaming caret on agent text and tool input. It is a *terminal* caret drawn as
text, it steps rather than fades, and it stops under reduced motion like
everything else. Do not use it anywhere a terminal is not being imitated.

Two things that look like they should move and deliberately do not, both
verified in code: the `Live` indicator is a static `●` plus the word "live" —
it does not pulse, because "live" is a property of the stream, not work in
flight, and a pulsing dot there is a decoration that says nothing a glyph and a
word do not — and a `Num` that changes value re-renders without a transition.
`running` is the only state that earns the pulse.

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

## Floating content appears in place

A tooltip, popover, menu, select or drop is positioned by a transform that
Radix sets after mount. It must never transition that transform, or the
panel slides in from the corner it was laid out at. `op.css` excludes
`[data-radix-popper-content-wrapper]` from the transform transition, and the
primitives carry no entrance or exit animation classes. Appear, then be
there.
