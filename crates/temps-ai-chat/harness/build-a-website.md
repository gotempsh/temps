---
name: build-a-website
description: Design a website, landing page or app screen so it looks made, not generated
requires_tool: sandbox_exec
contexts: assistant, project
---

# Build a website that looks made, not generated

Read this before writing any page people look at, together with the
`build-an-app` playbook. Left alone, every model ships the same page: Inter on
white, a centred hero, three feature cards, a purple gradient and a footer of
four link columns. This playbook exists to stop that. When the pages are
built, read the `check-a-website` playbook and pass it before you say they are
done.

## 1. Read what is already there

If the project has code, read it before designing: the fonts (`next/font`,
`@fontsource`, font links), the colour variables in the global stylesheet, the
component kit, and a `DESIGN.md` at the root. Keep an established system and
design inside it, and say in one line what you kept. A `DESIGN.md` overrides
everything below.

## 2. Ask once, or infer and say so

Before the first page of a new site, ask in the same `temps_ask_user` call as
any other design question (four questions at most):

- **Audience**: who uses it, and what they already know.
- **Use**: the one action the page should drive.
- **Tone**: an extreme, such as editorial, brutalist, soft, utilitarian,
  luxury, playful, technical or austere. "Clean and modern" is not a tone.

Tell them they can answer "go ahead". If they do, or the request already says
to just build it, infer all three from the request and state them in one
sentence at the top of your reply so they can redirect.

## 3. Pick the kind of page and a genre

A **marketing page** (landing, product, portfolio, docs home) follows every
step below. An **app screen** (dashboard, list, form, settings) takes the genre
and tokens, skips the page shape, and follows section 7.

Pick the genre from the request:

- **modern-minimal**: SaaS, API, developer tool, platform, B2B, dashboard. One
  sans family (Geist, or Inter Tight with Inter), near-monochrome with one
  restrained accent, pill or tight-radius buttons, subtle visible borders. Pure
  white and grey neutrals are fine here.
- **editorial** (when nothing else fits): publications, studios, food, craft,
  personal sites. A distinctive serif display (Fraunces, Instrument Serif,
  Newsreader) over a refined sans body, warm tinted paper, hairline rules.
- **atmospheric**: AI tools, music, video, voice, anything late-night. Dark
  paper that is not black, one luminous accent, at most two soft radial glows
  behind content and never on text.
- **playful**: consumer, community, family, onboarding. A rounded humanist sans
  (Plus Jakarta Sans, Nunito), a warm saturated accent, generous radius, one
  hand-drawn SVG motif.

## 4. Pick a page shape before writing code

Never ship hero, three feature cards, call to action, footer. Pick one shape:

- **Bento grid**: irregular tiles of different sizes holding features, a quote,
  a figure and a screenshot.
- **Workbench**: the product's own interface is the content, as a guided tour
  of using it.
- **Stat-led**: one real number leads, beside a headline that says what it
  means; the rest supports it.
- **Marquee hero**: one bold statement fills the first screen; below it the
  page becomes a list or a grid.
- **Split studio**: every block is a diptych, text on one side and proof on the
  other, alternating sides.
- **Narrative workflow**: numbered stages of how someone uses it over time.
- **Long document**: reads like a memo or a letter, prose with inline headings.
- **Conversational FAQ**: bold questions, short and honest answers.
- **Index-first**: the page is a beautifully set list of items or links.
- **Map or diagram**: one large diagram (a flow, a system, a map) organises the
  page.
- **Catalogue**: a uniform grid of variations, such as products, menus or plans.

Pick a navigation and a footer that suit the shape, for example a floating
pill, a masthead or a side rail, and a one-line or statement footer. The
default bar of logo, four links and a button, and the footer of four link
columns, are the two most recognisable generated tells.

## 5. State the plan before code

In five short lines: kind and genre; shape, navigation and footer; display and
body fonts; paper, ink and accent colours; and imagery (typography only, the
product's real interface, a hand-built SVG, or the user's own photos). When you
are proposing an architecture, put these lines in the message that ends that
turn, not in the proposal: its summary only says what the application does and
for whom. The user then approves both at once. Otherwise put them in your reply
and start. Do not wait a second time after an architecture was accepted.

## 6. Build with locked tokens

- **Tokens**: define every colour, font, spacing step, radius and easing once,
  as CSS variables in the global stylesheet, and use only those in components.
  No hex value or font name anywhere else. Add to an existing global
  stylesheet; never remove its `@import "tailwindcss"` line or the component
  kit's variables. Change their values, keep their names.
- **Colour**: OKLCH. Tint neutrals slightly toward the accent hue (except in
  modern-minimal). No pure black, and no pure white outside modern-minimal.
  The accent covers no more than about 5% of a screen: the primary action, the
  key figure, the focus ring.
- **Type**: at most three families (display, body, and mono for code only).
  Load each one, and point the font variables at the ones the loader defines.
  Headings are never italic, not even one word in them; emphasise with weight
  or the accent. Display line height 1.0 to 1.1 with tracking near -0.02em;
  body 16 to 18px with line height near 1.6; lines 45 to 75 characters wide. A
  headline you write has at most seven words. Numbers in tables use
  `font-variant-numeric: tabular-nums`.
- **Space**: a 4px scale. Give important sections more room than minor ones,
  and separate sections with a rule, a change of surface or a change of
  rhythm, not identical gaps.
- **Interaction**: every control has hover, active, disabled and
  `:focus-visible` styles; the focus ring appears instantly at 3:1 contrast or
  more. Transition named properties, never `transition: all`; 150 to 250ms,
  easing out, no bounce. Animate only transform and opacity, and turn motion
  off under `prefers-reduced-motion`.
- **Honest content**: copy from the user's domain and their data. Never invent
  a metric, testimonial, customer logo or count: use the real figure, a "—"
  marked "to confirm", or a different shape. No Jane Doe, no Acme, no lorem
  ipsum.
- **Imagery**: the strongest honest option: confident typography alone, the
  product's real interface rendered by its own components, or a hand-built SVG
  tied to the content. Never draw a fake browser bar, phone frame or window
  with three dots. No emoji as icons, and one icon set only.
- **Phones**: no sideways scroll from 320px wide (`overflow-x: clip` on both
  `html` and `body`); button and link labels never wrap onto two lines; grid
  tracks that hold images use `minmax(0, 1fr)`; headings get
  `overflow-wrap: anywhere`.
- **Stamp**: make the first line of the global stylesheet a comment naming the
  kind, genre, shape, fonts and accent, so the next change keeps the system or
  varies it on purpose.

## 7. App screens

Dashboards, tables and forms use the same tokens, plus:

- Content before decoration: no hero and no marketing copy. A page title, the
  primary action at the top right, then the data.
- Tables: text aligned left, numbers aligned right in tabular figures, quiet row
  dividers, a sticky header on long tables, and an empty state that says what
  to do next.
- Status as a small dot or tinted label from one shared set of states, never a
  rainbow of badges.
- Loading skeletons shaped like the content; errors say what failed and what to
  do about it.
- Recent times as relative ("5 min ago"), older ones as dates ("12 Feb 2026").
