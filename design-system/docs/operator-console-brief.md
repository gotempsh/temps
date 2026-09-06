# Operator console — design brief for further exploration

**Status:** the user has picked this as the primary direction to carry Temps' brand identity forward. This document hands off the current state to a more capable design/agent pass to generate further ideas — it is a brief, not a finished spec. Nothing here is final; the whole point is to get more/better options on the surfaces listed under "Open surfaces."

---

## 1. What Temps is

A self-hosted PaaS that replaces 6+ paid SaaS tools with a single Rust binary: deployment platform (Vercel), web analytics (PostHog/Plausible), session replay (FullStory), error tracking (Sentry), uptime monitoring (Pingdom), managed databases, and transactional email. Free to self-host; **Temps Cloud** is the managed/paid offering, not the product itself.

**Target user:** developers and teams who want Vercel-grade DX without vendor lock-in or SaaS sprawl — indie hackers, cost-conscious startups, and self-host-compliance enterprises.

**Three pillars the design has to serve** (see `temps/CLAUDE.md` and `design-system/src/sections/Brand.tsx`):
1. **Operator, not tenant** — the reader owns the machine this runs on. Copy and UI address someone debugging their own box at 2am, not a customer inside somebody else's SaaS.
2. **One binary, six tools** — deploys, analytics, replay, error tracking, uptime, email. No surface should look like it belongs to a different product.
3. **Free to self-host** — nothing should read as "upgrade to unlock" for capability self-host already has.

## 2. Why Operator console

Of five explored directions (Operator console, Editorial mono, Swiss grid, Ledger, Instrument panel), Operator console was picked because:
- True black/white, near-zero radius, monospace throughout — it's the only direction whose own description ("built by operators, for operators") is a paraphrase of the pillars above rather than a retrofit.
- It's the only one of the five that plausibly scales to *every* real product surface — dense tables, logs, forms — rather than just a hero or a status page. (Instrument panel's corner brackets and glow rings, for comparison, start fighting the content past a hero.)

The other four directions were reference points, not competing candidates; only Operator console was carried forward.

## 3. Current state — what actually exists today

### 3.1 Live reference (this repo)
- Standalone sandbox app, not wired into the real console: `temps/design-system/`, React 19 + Tailwind v4 + shadcn/ui.
- Operator console was prototyped as a token-only reskin in an exploration section (`OPERATOR_VARS`, `OperatorConsole()`); that exploration has since been superseded by the v5 skin and is no longer in the repo.
- It is currently a **pure CSS-custom-property reskin** of the exact same `Button`/`Badge`/etc. components used everywhere else in the sandbox — same DOM, different tokens. That's a deliberate constraint carried over from `temps/DESIGN.md`: extend primitives via tokens/props, never fork them.
- Only two surfaces have been mocked in this direction so far: a small deployments dashboard (metric tiles + status rows) and a landing hero (using temps.sh's real current copy). Everything else in section 5 below is unexplored.

### 3.2 Current token values (Operator console, both schemes)

```
dark:
  --background: oklch(0 0 0)          /* true black */
  --foreground: oklch(1 0 0)          /* true white */
  --card: oklch(0.07 0 0)
  --muted: oklch(0.14 0 0)
  --muted-foreground: oklch(0.6 0 0)
  --border: oklch(0.28 0 0)
  --primary: oklch(1 0 0) / --primary-foreground: oklch(0 0 0)
  --ring: oklch(0.72 0.19 150)        /* green-ish focus ring, NOT the brand blue — worth revisiting */
  --radius: 0.125rem                  /* ~2px, near-zero */
  font: 'JetBrains Mono' for --font-sans AND --font-mono (i.e. monospace for UI labels too, not just data)

light:
  --background: oklch(1 0 0)          /* true white */
  --foreground: oklch(0.08 0 0)
  (same structure, inverted)
```

Status colors used ad hoc in the mockup (not yet tokenized for this direction): `text-emerald-400`/`text-emerald-600` for OK, `text-amber-400`/`text-amber-600` for degraded.

### 3.3 Known open decision baked into the prototype

**Font mismatch.** The live product (`temps/web/src/globals.css`) uses Geist + Geist Mono (Vercel's actual fonts, explicitly credited in code comments as "Vercel-inspired palette (Geist design system)"). The Operator console mockup instead uses **JetBrains Mono** for everything, including UI chrome — an unrelated typeface picked for the exploration, never reconciled with what's actually shipped. Adopting Operator console for real means deciding: keep Geist Mono (continuity, but a Vercel-associated face) vs. switch to JetBrains Mono or something else entirely (stronger break from Vercel, but a bigger typographic migration and a new font dependency). This is probably the single highest-leverage open question — it touches every other decision below.

### 3.4 Brand rules that constrain everything

- Temps' brand is **white/black only**. Color is not the differentiation lever for this direction — density, type, and geometry are.
- Blue (`--ring`, `oklch(0.59 0.2032 256.82)` / Vercel's `#0070f3` in the real tokens) is reserved for **focus rings and links only** — never a fill, background, or button color. (Note: Operator console's prototype currently uses a green ring instead of blue — see 3.3-adjacent question below.)
- Green/amber/red are reserved for status semantics (ok/warn/error) only, not decoration.
- Mono type is for data/IDs/hashes/code only in the rest of the system — Operator console's premise of *all-monospace UI* is a deliberate exception to that rule, worth stress-testing at scale (does an all-caps monospace label wall get exhausting across a 40-row settings page? does it help or hurt scanability once real content — user names, arbitrary env var values, long URLs — replaces short synthetic sample data?).

## 4. What "good" looks like

The bar isn't "looks cool in a hero." It's: does this hold up as the *default* visual language for a dense, real operations product used daily? Concretely:
- A user should be able to work in this UI for hours without eye fatigue (true-black-on-white-text at high density is a real risk here — check contrast/fatigue, not just contrast ratio compliance).
- Density should read as competence, not clutter — every one of CLAUDE.md's actual complex surfaces (analytics dashboards with charts, session replay player, error tracking with stack traces, uptime monitoring timelines, deployment logs streaming in real time) needs a plausible answer in this language, not just the simple deployments-list demo built so far.
- Nothing here should require abandoning the underlying shadcn/Radix component set — the constraint is tokens and light structural variants, not a rewrite.

## 5. Open surfaces — where more ideas are wanted

None of these have been designed in Operator console yet. This is the actual ask: propose concrete treatments (with rationale, not just adjectives) for as many of these as possible, staying inside the token-reskin constraint from §3.1 and the brand rules from §3.4.

1. **Data visualization** — analytics charts, uptime timelines, error rate graphs, session replay scrubber. Monospace/terminal aesthetics don't have an obvious native chart language (terminal sparklines? ASCII-art-inspired? plain minimal line charts with square-cap strokes?). This is probably the second-highest-leverage open question after the font decision.
2. **Forms & validation** — multi-field forms (project settings, provider connection wizards), inline validation states, required/optional markers, help text placement. Near-zero radius + monospace labels at form-density needs a concrete pass, not just a button restyle.
3. **Modals & dialogs** — including the type-to-confirm destructive-delete pattern — does Operator console change how that should look/feel (e.g. a terminal-confirmation metaphor)?
4. **Empty states & onboarding** — first-run experience, unconfigured-feature onboarding (a hard requirement per `temps/CLAUDE.md`: unconfigured features must onboard with a concrete example + setup link, never disappear). What does that look like in a terminal-flavored UI without feeling like an error message?
5. **Notifications & toasts** — transient feedback for actions (deploy started, backup completed, etc.) in a true-black/white system with minimal color.
6. **Command palette / keyboard-driven interaction** — Operator console's whole premise ("operators, not tenants") suggests keyboard-first UX is more on-brand here than anywhere else in the product. Is there a natural command-palette or `⌘K` treatment that fits, and should it be a signature interaction for this direction specifically?
7. **Log/code viewers** — deployment build logs (JSONL, streamed live), error stack traces, session replay console output. This is arguably where "monospace everywhere" already has the strongest natural fit — but line numbers, syntax highlighting, log-level coloring, and search/filter chrome haven't been touched.
8. **Settings/table-dense pages** — the original complaint that started this whole design-system effort was inconsistent, oversized cards on pages like Domains and Git Providers. Operator console needs a concrete answer for a 50-row settings table, not just the 2-row demo built so far.
9. **Iconography** — currently just default `lucide-react` at default stroke width, unchanged from every other direction. Does an operator-console identity want a different stroke weight, a monochrome-only icon treatment, or literal ASCII/glyph-based icons instead of line icons?
10. **Motion** — nothing about transitions, loading states, or animation has been considered in any direction yet. Does "operator console" want abrupt/instant state changes (terminal-like, no easing) as part of its identity, or does it need conventional motion for usability?
11. **Responsive/mobile behavior** — all exploration so far has been desktop-width mockups only.
12. **Accessibility** — true black (`oklch(0 0 0)`) against true white text is contrast-compliant by the numbers, but worth an explicit pass: focus-visible states, reduced-motion, and whether the near-zero radius + thin borders read clearly for low-vision users.

## 6. Hard constraints (do not relitigate these)

- Must extend the existing shadcn/Radix component primitives via CSS custom properties and/or a small prop/variant (see `tabs.tsx`'s `TabsVariant` pattern in this repo for the sanctioned way to do this) — never fork a component wholesale.
- Brand color stays monochrome; no new hues beyond the reserved blue-for-focus/links and green/amber/red-for-status.
- Whatever is proposed needs a plausible migration path from the *actual* current live tokens (Geist/Geist Mono, Vercel-styled palette in `temps/web/src/globals.css`) — a total rewrite proposal is fine to describe, but should acknowledge what it costs to move off the current system.
- The real logo (`temps-icon.svg` / `temps-icon-dark.svg`, copied into this sandbox at `design-system/public/logo/`) is a fixed two-color asset, not a themeable token — any hero/nav treatment needs to work with it as-is, not assume it can be recolored.

## 7. Reference material

- Live sandbox: run it locally with `bun run dev` (see the README), or point at your own host, e.g. `http://<your-temps-host>:5183/`
- Brand rationale page: `temps/design-system/src/sections/Brand.tsx`, and the live route `/brand`
- Canonical design doc: `temps/DESIGN.md`
- Real current (live) tokens: `temps/web/src/globals.css`
- Real logo assets: `temps-landing/public/favicon.svg`, `temps-landing/public/logo/temps-icon*.svg`
- Real landing page for tone/copy reference: https://temps.sh

## 8. What to hand back

Concrete proposals per open surface (§5), each with: what it looks like, why it fits the operator/monochrome/dense-tool identity, and what it costs to build (new component vs. pure token change). Where two options are genuinely close, present both with a recommendation rather than picking silently — the font decision in particular (§3.3) should come back with an explicit recommendation, since it gates several of the others.
