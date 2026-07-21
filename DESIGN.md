# Temps Web — Design System

Single source of truth for UI/UX decisions in `temps/web/`. If a pattern isn't
here, it isn't standard. Prefer editing this file over inventing a one-off.

- **Stack:** React 19 · TypeScript · Rsbuild · Tailwind v4 · shadcn/ui (Radix)
- **Tokens:** defined in `src/globals.css` via CSS variables, consumed through
  Tailwind utilities (`bg-background`, `text-muted-foreground`, etc.)
- **Icons:** `lucide-react` only
- **Forms:** `react-hook-form` + `zod`
- **Data:** TanStack Query — use `isPending` / `isLoading` / `isError`, never
  manual `useState` for loading

> **Rule of thumb:** if a color, spacing, or radius value is hardcoded and not
> a token, stop and use a token instead.

---

## 1. Design principles

1. **Content over chrome.** Cards, borders, and shadows exist to group
   content, not decorate it. Default to flat surfaces; add depth only when
   elevation has meaning.
2. **Tokens over values.** No hex codes, no raw `rgb()`, no one-off radii.
   Everything flows from the tokens in §3.
3. **One obvious next action.** Every page has a primary CTA. Secondary and
   tertiary actions step down in visual weight.
4. **Feedback for every action.** Toast (`sonner`), inline error, or state
   change — never silent.
5. **Progressive disclosure.** Don't show settings, options, or data the user
   doesn't need at this step. Use dialogs, accordions, and detail routes.
6. **Tabular numbers for numbers.** Use `tabular-nums` anywhere digits may
   change (metrics, pagination, timestamps).
7. **Mobile-first, desktop-great.** Never hide critical actions behind
   overflow on mobile. On desktop (>`xl`), constrain width with `max-w-7xl`.

---

## 2. Typography

The app uses a **Geist-based** type system — `Geist` for UI/body (via
`--font-sans`) and `Geist Mono` for code, IDs, and anything requiring
monospace (via `--font-mono`). This matches Vercel's product surfaces for a
professional, modern dashboard feel. Do not introduce additional font
families without updating this doc and `globals.css`.

| Role             | Classes                                                      | Notes                              |
| ---------------- | ------------------------------------------------------------ | ---------------------------------- |
| Page title (H1)  | `text-2xl font-semibold tracking-tight`                      | One per page, in the page header   |
| Section (H2)     | `text-base font-semibold tracking-tight`                     | Above a group of cards/lists       |
| Card title       | `font-semibold leading-none`                                 | Inside shadcn `CardTitle`          |
| Body             | `text-sm` (default in cards/forms), `text-base` (prose only) | Geist sans — never `font-mono`     |
| Muted / meta     | `text-xs text-muted-foreground`                              | Timestamps, helper text, captions  |
| Label (uppercase)| `text-xs font-medium uppercase tracking-wide text-muted-foreground` | Metric card titles, table headers |
| Metric value     | `text-2xl font-semibold tracking-tight tabular-nums`         | KPIs                               |
| Code / IDs       | `font-mono text-xs`                                          | Commit hashes, IDs, paths, tokens — **only** place `font-mono` is allowed |

**Rules**
- Never use `font-bold` — prefer `font-semibold` for emphasis.
- Never apply `font-mono` to body copy, labels, titles, or buttons. Reserve
  it for code fragments, IDs, hashes, file paths, and CLI output.
- Never hardcode colors on text — use `text-foreground`, `text-muted-foreground`,
  or semantic tokens (`text-destructive`, `text-emerald-600 dark:text-emerald-400`).
- Truncate long strings with `truncate` on a `min-w-0` parent.

---

## 3. Color tokens

Colors are defined in `src/globals.css` as OKLCH CSS variables, exposed to
Tailwind as `bg-*`, `text-*`, `border-*`. **Never use literal colors** except
for the sanctioned status hues in §3.2.

### 3.1 Semantic surfaces

| Token                | Use                                                 |
| -------------------- | --------------------------------------------------- |
| `background`         | Page background                                     |
| `foreground`         | Primary text                                        |
| `card`               | Elevated content surface (cards, metric cards)      |
| `muted`              | Subtle fills (skeletons, locked states, hover rows) |
| `muted-foreground`   | Secondary text, icon defaults                       |
| `accent`             | Hover backgrounds for nav, menu items               |
| `border` / `input`   | All borders and form control outlines               |
| `ring`               | Focus rings only — never decorative                 |
| `popover`            | Dropdown / menu / tooltip surfaces                  |
| `primary`            | Default button, solid emphasis                      |
| `secondary`          | Secondary button, quiet emphasis                    |
| `destructive`        | Errors, destructive actions                         |
| `sidebar*`           | All sidebar surfaces — do NOT reuse these elsewhere |

### 3.2 Status colors (sanctioned literals)

Only these literal hues are allowed, and only for status meaning — never
decoration. Always provide both light and dark variants.

| Meaning       | Light                    | Dark                              |
| ------------- | ------------------------ | --------------------------------- |
| Success / up  | `text-emerald-600`       | `dark:text-emerald-400`           |
| Warning       | `text-amber-600`         | `dark:text-amber-400`             |
| Error / down  | `text-red-600`           | `dark:text-red-400`               |
| Info          | use `text-muted-foreground` (don't add blue) |               |
| Neutral / off | `text-muted-foreground`  |                                    |

Status **dots** use solid fills:
`bg-emerald-500` · `bg-amber-500` · `bg-red-500` · `bg-zinc-400`
(size: `h-2 w-2 rounded-full`).

---

## 4. Spacing & layout

Tailwind spacing = 0.25rem (4px) base. Stick to the scale; no arbitrary values
unless aligning to a specific pixel asset.

### 4.1 Page wrapper

Every top-level page uses the same **full-width** wrapper — content fills the
available space next to the sidebar rather than being capped and centered.
Readability of individual blocks is handled per-section (grids, `max-w-*` on
prose/forms where a narrow measure helps), not by constraining the whole page.

```tsx
<div className="w-full p-4 sm:p-6 lg:p-8">
  {/* content */}
</div>
```

Do **not** wrap a page in `mx-auto max-w-7xl` (or similar) — that leaves large
empty gutters on wide displays. If a specific block reads better narrow, cap
that block, e.g. a settings form in `<div className="max-w-2xl">`.

### 4.2 Vertical rhythm

| Gap              | Class     | Use                                                |
| ---------------- | --------- | -------------------------------------------------- |
| Within a card    | `gap-2` / `space-y-1` | Tight groups (title + meta)           |
| Between fields   | `gap-4` / `space-y-4` | Form fields, stacked inputs           |
| Between sections | `space-y-6` (mobile) `space-y-8` (desktop) | Section groups   |
| Below page header| `mb-6 sm:mb-8`        | Standard                              |

### 4.3 Grids

| Context           | Classes                                                       |
| ----------------- | ------------------------------------------------------------- |
| Metric cards      | `grid gap-4 grid-cols-2 lg:grid-cols-4`                       |
| Project / entity  | `grid gap-4 md:grid-cols-2 xl:grid-cols-3`                    |
| Stat tiles (dense)| `grid gap-3 grid-cols-2 md:grid-cols-4`                       |
| Detail page       | `flex flex-col lg:flex-row gap-6` (main + side panel)         |

### 4.4 Page header pattern

```tsx
<div className="mb-6 flex flex-col gap-4 sm:mb-8 sm:flex-row sm:items-end sm:justify-between">
  <div className="space-y-1">
    <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
    <p className="text-sm text-muted-foreground">{subtitle}</p>
  </div>
  <div className="flex items-center gap-2">
    {/* filters, then primary CTA last */}
  </div>
</div>
```

Primary CTA is always right-most on desktop, full-width on mobile.

---

## 5. Radius & elevation

### Radius
Base token: `--radius: 0.5rem`. Use Tailwind radius classes:
- `rounded-sm` (xs inputs, dots) · `rounded-md` (inputs, small cards) ·
  `rounded-lg` (cards, panels, buttons) · `rounded-xl` (large hero surfaces) ·
  `rounded-full` (avatars, status dots, chips)

Never use arbitrary radii like `rounded-[7px]`.

### Shadows
- Default surface: **no shadow**. Use `border` for separation.
- Floating surfaces (dropdowns, popovers, dialogs): use the component default
  (shadcn applies `shadow-md` or `shadow-lg` internally).
- Never stack shadows. Never use colored shadows.

---

## 6. Components — standards

All primitives live in `src/components/ui/`. **Do not fork them**; extend via
props or wrap in a feature component.

### 6.1 Buttons
Use shadcn `Button` + `variant` / `size`. Map intent → variant:

| Intent         | Variant         | Example                        |
| -------------- | --------------- | ------------------------------ |
| Primary action | `default`       | "New project"                  |
| Secondary      | `outline`       | "Cancel", filter toggles       |
| Quiet / inline | `ghost`         | Icon buttons in toolbars       |
| Destructive    | `destructive`   | Delete, disconnect             |
| Link           | `link`          | In-text navigation             |

Sizes: `sm` in toolbars and row actions, default on primary CTAs, `icon` for
icon-only (must include `<span className="sr-only">`).

**Icon + label:**
```tsx
<Plus className="mr-1.5 h-4 w-4" />
<span className="hidden sm:inline">New project</span>
<span className="sm:hidden">New</span>
```

### 6.2 Cards
```tsx
<Card>
  <CardHeader>
    <CardTitle>...</CardTitle>
  </CardHeader>
  <CardContent>...</CardContent>
</Card>
```
- Hover-linked cards wrap the card in `<Link>` and add
  `hover:bg-muted/50 transition-colors` on the `Card`.
- `CardContent` default padding is fine; override with `p-4` for dense grids.
- Keep card anatomy predictable: avatar/thumbnail (left) · title + meta
  (center) · actions/badge (right).

### 6.3 Metric cards
Use the shared `components/dashboard/MetricCard.tsx`. Never build an ad-hoc
metric card — add a prop instead.

Pattern:
- Uppercase tracked label (§2)
- Large tabular-nums value
- Trend line: icon + `vs prev. period`
- Locked state: `locked` prop renders a "Coming soon" chip + em-dash value
- Optional `sparkline` slot

### 6.4 Badges
shadcn `Badge` variants: `default` · `secondary` · `destructive` · `outline`.
- `outline` for neutral tags (branch, environment)
- `secondary` for counts
- `destructive` only for errors
- Never put more than 3 badges in a row — collapse into a popover.

### 6.5 Forms

- Wrap with `<Form>` from shadcn + `react-hook-form`.
- Validate with `zod` — schema co-located with the form component.
- Labels: always present, above the input. No placeholder-as-label.
- Help text: `text-xs text-muted-foreground` below the input.
- Error text: `text-xs text-destructive` below the input.
- Submit buttons reflect loading state (`disabled={mutation.isPending}` +
  spinner icon).

### 6.6 Tables

- Wrap in `<div className="overflow-x-auto">`.
- First column: identifier with link. Last column: actions (right-aligned).
- Hide secondary columns on small screens: `hidden md:table-cell`.
- Zebra striping: do **not** use. Rely on row hover (`hover:bg-muted/50`).
- Empty state uses `EmptyPlaceholder`.

### 6.7 Pagination

Standard footer below a list:

```tsx
<div className="flex flex-col gap-2 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
  <p className="text-xs text-muted-foreground tabular-nums">
    <span className="hidden sm:inline">Showing {from}–{to} of {total}</span>
    <span className="sm:hidden">Page {page} / {totalPages}</span>
  </p>
  <div className="flex items-center gap-2">
    <Button variant="outline" size="sm" disabled={page === 1}>Previous</Button>
    <Button variant="outline" size="sm" disabled={page >= totalPages}>Next</Button>
  </div>
</div>
```
Hide entirely when `total <= perPage`.

### 6.8 Dialogs & sheets
- Use `Dialog` for confirmations and short forms.
- Use `Sheet` (right side) for multi-field edit surfaces that benefit from
  page context behind them.
- Title is mandatory; description is mandatory when the action is
  destructive.
- Primary action is right-most; destructive uses `variant="destructive"`.

### 6.9 Empty states
Always use `<EmptyPlaceholder>` with:
- An icon from lucide
- Title (what's missing)
- Description (one sentence, what to do)
- One primary CTA (optional)

Never ship a blank region.

### 6.10 Loading states

- **Query-driven lists:** skeletons that mirror the final layout
  (`ProjectCardSkeleton`, `MetricCardSkeleton`). Never a centered spinner for
  a full page.
- **Mutations:** button spinner + disabled state. Toast on success/error.
- **Background refresh:** do not show a loader; let cached data stay.

### 6.11 Copy-to-clipboard
Always use `<CopyButton>`. Never wire `navigator.clipboard` by hand.

### 6.12 Sidebar navigation

The sidebar is a single column with one active context at a time. Two
patterns only:

- **Flat list.** Top-level items navigate directly. Use this for the
  workspace root, settings root, and project root.
- **Drill-down sub-view.** When a parent has its own dense sub-tree
  (more than ~3 children, or the children form a coherent surface),
  clicking the parent **replaces the entire sidebar** with the
  sub-view: a back arrow + section title at the top, then the
  sub-items as a flat list. Back returns to the previous view without
  changing the page route.

**Drill-down items must have icons.** Every sub-item in a drill-down
view renders a leading lucide icon (`h-4 w-4`) next to its label —
matching the flat-list nav pattern. Icon-less text lists (e.g.
"Analytics / Visitors / Pages / Funnels / Session Replays / Uptime /
Metrics / Request Logs / Traces / Speed") are visually ambiguous and
force users to read every label to scan. Pick an icon that reflects
the destination's purpose; reuse the same icon the destination page
uses in its header when one exists.

**Do not** expand sub-items inline with a chevron / accordion. Inline
accordions create double-scroll, hide context from the rest of the
nav, and don't scale past a couple of items. If a section has enough
functionality to need grouping, it has enough to deserve its own
sub-view.

The route-driven swap between workspace, settings, and project nav in
`Sidebar.tsx` is the canonical example.

---

## 7. Iconography

- Library: `lucide-react` only.
- Inline sizes: `h-4 w-4` inside buttons/badges, `h-5 w-5` for page-level
  icons (empty states, headers).
- Color: inherit from parent text color. Don't set `text-*` on an icon
  directly unless it's a status icon.
- Pair icons with text labels on mobile for primary actions; icon-only is OK
  on desktop toolbars when the tooltip exists.

---

## 8. Motion

- Transitions: `transition-colors` on hoverable surfaces, `transition-opacity`
  for reveals. Default duration (150ms) is fine — don't override.
- Never animate layout-shifting properties (width/height/margin) on hover.
- Dialog/popover motion comes from `tailwindcss-animate`; don't write custom
  keyframes for standard overlays.

---

## 9. Responsive breakpoints

| Prefix | Min width | Use                                       |
| ------ | --------- | ----------------------------------------- |
| (none) | 0         | Mobile base                               |
| `sm`   | 640px     | Phone landscape / small tablet            |
| `md`   | 768px     | Tablet — activate 2-col grids             |
| `lg`   | 1024px    | Laptop — sidebar expanded, 3-col safe     |
| `xl`   | 1280px    | Desktop — up to 4-col dashboards          |
| `2xl`  | 1536px    | Ultrawide — content still capped at `max-w-7xl` |

### Mobile rules (enforced)

- Tables: `overflow-x-auto`, hide secondary columns with `hidden md:table-cell`.
- Filter bars: `flex flex-col gap-2 sm:flex-row sm:flex-wrap`. Selects:
  `w-full sm:w-[Npx]`.
- Side panels: `flex-col lg:flex-row`, panel uses `w-full lg:w-[Npx]`.
- Headers: `flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between`.
- Button labels: `hidden sm:inline` next to icons.
- Pagination: compact `{page} / {totalPages}` on mobile.

---

## 10. Dark mode

- Scheme toggled by `next-themes` on `<html class="dark">`.
- Every semantic token has a dark variant in `globals.css` — if a component
  uses tokens, it's automatically correct in both modes.
- Sanctioned status literals must ship both variants:
  `text-emerald-600 dark:text-emerald-400`, etc.
- Test every new surface in both modes before shipping.

---

## 11. Copywriting

- **Sentence case** for titles and buttons: "New project", not "New Project".
- **Short and direct.** "Delete project" beats "Remove this project permanently".
- **No emoji** in UI text.
- **Numbers:** `toLocaleString()` for counts over 999. Use `tabular-nums`.
- **Dates:** relative via `<TimeAgo>`; absolute tooltips only when precision
  matters.
- **Status:** "Operational / Degraded / Down / Unknown" — mirror backend
  enums exactly.
- **Empty states:** explain the concept, then the action. "No projects yet.
  Connect a repo to deploy your first one."

---

## 12. Anti-patterns (do not ship)

| Don't                                                 | Do instead                                    |
| ----------------------------------------------------- | --------------------------------------------- |
| Hardcoded hex / rgb colors                            | Tokens from §3                                |
| Centered spinner for full-page load                   | Skeleton matching final layout (§6.10)        |
| Manual clipboard handler                              | `<CopyButton>`                                |
| Toast AND inline error for the same failure          | Pick one — inline for field errors, toast for global |
| IIFE inside JSX (`{(() => {...})()}`)                 | Helper function or sub-component              |
| Hooks after early returns                             | All hooks before any `return`                 |
| Conditional mounting of stateful dialogs/sheets       | Always mount, use `open` prop                 |
| Fake data overlaid with "Coming soon"                 | `locked` state with em-dash + chip            |
| Dropdown when fewer than 5 options                    | Segmented control / radio cards               |
| Cards inside cards                                    | Flatten or use a section heading              |
| Custom shadows or radii                               | §5 tokens                                     |
| Collapsible / accordion sidebar items                 | Drill-down sub-view (§6.12)                   |
| Icon-less labels in drill-down sidebar sub-views      | Every sub-item gets a lucide icon (§6.12)     |

---

## 13. Adding something new

1. **Check this doc first.** 80% of the time the pattern exists.
2. **Check `src/components/ui/`** for a shadcn primitive.
3. **Check `src/components/<domain>/`** for an existing domain component.
4. If none fit, build it using tokens and the rules above. If the new
   pattern is reusable, add a section to this doc in the same PR.
5. Flag any token addition (`--color-*`, new radius, new font) in PR
   description — those require review.

---

## 14. Reference surfaces

Canonical examples to pattern-match against:

| Surface          | File                                                    |
| ---------------- | ------------------------------------------------------- |
| Page wrapper + header | `src/pages/Dashboard.tsx`                          |
| Metric card      | `src/components/dashboard/MetricCard.tsx`               |
| Entity card (link)| `src/components/dashboard/ProjectCard.tsx`             |
| Sidebar / nav    | `src/components/dashboard/Sidebar.tsx`                  |
| Top header       | `src/components/dashboard/Header.tsx`                   |
| Empty state      | `src/components/EmptyPlaceholder.tsx`                   |
| Tokens           | `src/globals.css`                                       |

When in doubt, copy the closest canonical surface and adapt — don't invent.
