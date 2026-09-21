# @temps-sdk/ds — RULES

Imperative digest. If a rule and a screenshot disagree, fix the screenshot.

## Setup

- Depend on `@temps-sdk/ds` (workspace) and import from it, not from `web/src`
  directly, for anything the package exports.
- Never import `.tds` scoping into `:root`. Add the class to the subtree that
  opts in (the sandbox's `<body>` does this). It must never fight the host
  app's own theme.
- Base primitives (Button, Badge, Input, Card, Table, Command, AlertDialog…)
  come from `@temps-sdk/ui`. Do not re-import shadcn separately.

## Non-negotiable

- Every screen is built from a template (`Ledger`, `Detail`, `Settings`) or a
  primitive on top of one. Hand-rolled `flex items-center justify-between`
  page headers are a bug, not a style choice.
- An unconfigured feature always renders a surface: what's missing, a
  concrete example, a link to the settings page. Never nothing. `PageState`
  variant `not-set-up` enforces this at the type level — `requirement`,
  `example`, `settingsHref` are required props.
- Color never decorates. It appears only through `Status` (badges, dots,
  chart series). If you reach for a raw color to convey meaning outside
  `Status`, stop and add a tone to `Status` instead. Existing provider logos
  are a narrow identity exception: keep their brand color inside the small
  mark, never on the surrounding card, selection border, or action button.

## Tokens

- Colors, radius, spacing, type, shadows: `tokens.json`, consumed via
  Tailwind's semantic classes (`bg-success`, `text-muted-foreground`,
  `rounded-md`) — never a literal.
- `bun run lint` runs `tokens:check`, which fails if `tokens.json`,
  `src/tokens.css`, and `web/src/globals.css` disagree.

## Banned

- Raw hex (`#0070f3`) or `oklch(...)` literals outside `tokens.json`/`src/tokens.css`.
- Raw `px`/`ms` literals in components — use the Tailwind scale or a token.
  `audit-records.mjs` catches both; `// audit-ignore` requires a reason.
- `disabled` on a button mid-action — use `Button`'s `busy`/`busyLabel`.
- Toasts for confirmation of a click the user is still looking at
  (`CopyAction` answers on itself). Toasts are for events the user isn't
  currently watching.

## Type

Geist / Geist Mono (unchanged tokens). One `h1` per page, via `PageHeader`.
Never a second `h1` on the same route.

## Motion

Reuse the utilities already in `globals.css` (`animate-in`, `fade-in-50`,
`animate-spin`, etc.) — do not hand-write a new keyframe for something those
already cover. Respect `prefers-reduced-motion` (already handled globally).

## Status vocabulary

Five tones only: `ok` `warn` `error` `idle` `running`. Dot + word, in a Badge
by default (`variant="dot"` for dense rows). Never invent a sixth tone or a
glyph vocabulary — extend `STATUS_TONES` in `status.tsx` and update this file
in the same change if the app genuinely distinguishes a new state.

## Icons

Use `lucide-react` for interface actions and resource types. Use the existing
provider logos through `GitProviderMark` for Git provider identity — a branch
icon describes a branch, not GitHub or GitLab. Provider marks retain their existing brand colors by default. Use
`variant="monochrome"` when identity is secondary; it inherits the current
text color. Always pair a selection mark with the provider name.

Use `size-4` in controls, `size-5` for provider identity, and `size-3.5` for
dense metadata. No raw dimensions. Pair icons with visible text; hide redundant
icons from assistive technology. Icon-only controls need an accessible action
name on the button. A standalone provider mark needs its `label` prop.

Use `Status` for outcomes. Wizard numbers show sequence, a neutral check means
completed, and selection uses a check plus a visible border and `aria-pressed`.
Do not give providers status colors or use decorative icons as step numbers.
See sandbox `/iconography` for examples.

## Page structure

`PageContainer` owns horizontal padding — nothing else in a page tree sets
`px-*` on its outer wrapper. `PageHeader` owns the `h1` + actions row.

## Record page checklist

Title → verdict (`Status`) → 4-6 facts → main column → aside. If you have
more than 6 facts, the rest belongs in `main`, not the fact grid. `aside` is
optional; `main` is not.

## Forms

`Field` for every control (label + control + description + error, wired
`aria-describedby`). `FormErrors` above the sticky save bar, not only inline. Error summaries identify
the field as well as the problem. Pass Field props through `Picker inputProps`
so searchable controls retain label/help/error associations.
`Settings`'s save bar is always mounted — it doesn't appear/disappear with
`dirty`, so its position never jumps.

## Notifications

In-page state (validation, "not set up", empty) → `Callout`/`PageState`.
Confirmation of the user's own click → the control itself (`CopyAction`).
Background events the user isn't watching → `sonner` toast (unchanged,
outside this package's scope).

## Data

Numbers, bytes, durations, relative/absolute time → `fmt.ts`. No hand-written
`toFixed`/`Intl` calls scattered across screens.

## Collection states

Keep the section heading and filter controls mounted while data changes.

- First request pending: `DataTable isLoading`, with an accessible table name
  (`aria-label`). Keep the real column headings; never show empty copy yet.
- Successful response with no records: compact `PageState` `empty`, explaining
  what creates the first record.
- No filter matches: compact `PageState` `empty`, naming the active filter and
  offering a working clear-filter action. Do not imply no records exist.
- Request failed with no usable data: compact `PageState` `failed`, naming what
  could not load and providing a retry of that request.
- Background refresh failed: keep the last usable rows and filters visible;
  add a `Callout` explaining the data may be stale, with a retry action.
- Multiple independent requests: one panel's failure must not erase another
  panel's usable content. Use separate query states.

Keep errors, empty content, and loading in the collection's usual location.
Do not nest a second bordered empty-state panel inside a table surface.
`DataTable` owns its border and horizontal scroll; `PageState` can replace
it or sit inside an existing surface without adding another border.

Example: sandbox `/table-states` has shareable scenarios, working retries,
and filtering. Production reference: `CronJobDetail.tsx`.

## Overview filters

Keep search, result selectors and date-time controls in one compact, wrapping
row above the overview. Use `TimeRangeFilter` for URL strings or
`DateTimeRange` for controlled timestamp values. Default shortcuts are
1h / 6h / 24h / 7d; custom ranges commit only on Apply, show the local time
zone, validate ordering, and respect the data source's maximum window.
Preserve exact custom timestamps in the URL. Relative shortcuts remain
relative to now when reopened.

An overview's metrics, chart and table must derive from the same filters.
Reset pagination when filters change. Show no rate or percentile when there
are no observations; never substitute a misleading 100% or zero duration.
Keep demo/debug controls outside the normal filter toolbar.

## Charts

`TimeChart` (wraps `ThresholdLineChart`) for every time series. Don't hand-roll
a new recharts panel — extend `TimeChart`'s props if it's missing something.

## Content

No vanity data, no placeholder customer names/hostnames in fixtures (repo-wide
rule — see the worktree's `CLAUDE.md`). Empty and not-set-up copy states the
real requirement, not "Nothing here yet."

## Keyboard

Shortcuts get a visible `Kbd` badge next to the control they trigger, never a
keyboard-only entry point. `Picker` is filterable by keyboard from focus.

## Wizards

Use `Wizard` for a sequence of decisions. Its `PageHeader` stays aligned
with the step surface; progress labels remain readable on small screens. Put the
current step in a defined surface and actions in the `footer` slot. Each
step needs a descriptive heading, labeled inputs or choices, and a clear
next action. Explain what is missing before Continue can act. Allow Back
without discarding selections. Completion names the outcome and offers a
useful next step; sample flows must not imply a real resource was created.

`Wizard` with a footer defaults to a centered column at 80% of the available
width on desktop and full width on smaller screens. Keep
context beside the relevant input vertically, and group Back and the primary
action beneath the form. Do not stretch a short form, summary, and actions
across the viewport. Use `fullWidth` only for steps whose content needs it.

`Wizard` does not own gutters: wrap standalone pages in `PageContainer`.
Keep existing embedded consumers' surrounding containers. Celebration is
opt-in, never needed to communicate success; use a text-labeled `Status`.

## Responsive

`PageHeader` actions wrap to a second row before they overflow. `Detail`'s
aside stacks below `main` under `lg`. Tables scroll horizontally before they
break layout — never let a fact grid or button row force horizontal page
scroll.

## Requirements: the URL is the state

Filters, tab, page number, selected record: `useUrlState`, not local
`useState`. A refreshed or shared link must reproduce what the user saw.

## Before you ship

1. `bun run lint` passes (typecheck + tokens:check + audit:records).
2. Every "not set up" surface names the missing config and links to it.
3. No raw color/px/ms literal outside tokens — the audit catches most of
   this, but re-scan by eye for anything the regex can't (inline `<style>`,
   a `boxShadow` object).
4. The record recipe order (title → verdict → facts → main → aside) holds.

## Progressive disclosure

Show only what the user needs for the next action. Page descriptions are optional;
omit copy that repeats a title, label, or obvious control. Keep field-format
requirements next to the field. Prefer one short sentence over a paragraph.

Use `HelpPopover` for optional, short context and `Disclosure` for longer technical
details or advanced settings. Name the topic in the trigger ("Retention details",
not "Learn more"). Help opens by click or keyboard, never hover alone.
Do not nest help buttons inside a label or another interactive control.

Never hide validation, actionable warnings, destructive consequences, or missing
setup requirements. Do not collapse frequently used controls. Keep a visible
sample label in demos, and move walkthrough instructions into "About this example"
outside the main workflow. Use existing semantic spacing and colors; help is not
a new bordered card around every field.

Use `SettingsSection` for collapsible form groups, not the informational
`Disclosure`: it retains field values and reveals validation errors. Use open sections for everyday settings, with section headings on the left and
controls on the right on desktop; stack headings above controls on mobile. Separate groups with
spacing instead of a card or divider around each one. Reserve collapsible form
groups for advanced or infrequent controls. Keep short forms at a readable width
(the settings reference uses a `max-w-5xl` grid with one-third headings and
two-thirds controls) rather than stretching inputs across the viewport. `Field.help` places optional context beside the label;
required instructions and errors remain visible below the control.

Use `SettingsGroup` for the aligned open layout. Use `SettingsSection` only when
the form group should collapse; each caller retains its own save scope.

SettingsGroup descriptions are optional: one short sentence beneath the heading
that clarifies scope or outcome. Omit repeated labels and detailed instructions.

## Resource detail navigation

- Use `RecordLink` for the resource name in a table's identity column. Its
  underline and right arrow are always visible, including on touch screens.
- No row-click navigation, overlay links, or duplicate View/Details buttons in
  the actions column. `DataTable` and `Ledger` intentionally have no `onRowClick`.
- Keep selection, expansion, copy, menus, Edit, and Delete independently operable.
  Use a labeled button for expansion; row whitespace does not navigate.
- Use real routed URLs for details/configuration; preserve Enter, modifier-click,
  new-tab, refresh, and browser history. Do not replace links with buttons.
- Records without a detail route use plain text, with no navigation arrow.
- Detail facts, checks, forms, and history fill the parent width. No page-level
  `max-w-*`; reserve aside space only when an aside exists.
- Migrate touched legacy tables to this pattern. Reference: sandbox `/ledger`,
  production environment-variable table, and root `DESIGN.md` section 4.


### Shared breadcrumbs

Use the application's general breadcrumbs in the dashboard `Header`, populated
through `useBreadcrumbs` from `@/contexts/BreadcrumbContext`. Do not render a
second breadcrumb trail inside a page or assemble local links with slash or
chevron separators. The design system's breadcrumb primitives are for the shared
renderer, not a separate page-level navigation pattern.

The route/layout owning a trail must include linked ancestors and a non-linked
current page, with human-readable resource names. Nested routes extend the trail
(e.g. Projects → project → Environment variables → GITHUB_TOKEN → Check
configuration). Keep one owner per trail to avoid parent/child effects overwriting
each other. Update it on direct entry, refresh, back/forward, resource renames,
and return to the list; loading, unavailable, and missing records need safe labels.
Never include secret values. Keep full-width page content below the shared header.


### Detail pages with ongoing activity

Keep compact resource facts above URL-backed Checks and History tabs. Current
health is the default view; a growing audit log must not extend that view.
Use aligned tables, explicit status labels with icons, and expandable diagnostic
findings. Sort checks needing attention first. Paginate checks and history using
`ResponsivePagination`; preserve the selected view and page in URL parameters.
Fetch history when its tab is opened. Use the shared page header, breadcrumbs,
and full content width. Environment-variable details are the reference example.


### Canonical tabs: underline navigation

Use the shared `Tabs`, `TabsList`, `TabsTrigger`, and `TabsContent` exported by
`@temps-sdk/ds` (and `@temps-sdk/ui`) for peer page views. The default is a
transparent, full-width strip with a bottom divider and an underline on the
active tab. No pill container, selected-card background, shadow, or page-local
styling overrides. Keep labels text-first, with constant font weight.

Use `TabsTrigger count={number}` for optional counts, including zero. Omit a
count until it is known; never invent totals or show the current page's item
count as the total. Counts stay visible on inactive tabs. Keep Radix keyboard
navigation, focus indicators, disabled states, and panel semantics; let long
strips scroll horizontally. Meaningful detail views keep their selection in
the URL. Use a segmented toggle only for a local value choice (such as chart
interval or list/grid display), not for navigating content sections.

The shared primitive applies this decision to its existing consumers. Remove
legacy style overrides when touching a screen. The design-system Components
page and environment-variable detail page are reference implementations.

### Credential provider logos

Use `CredentialProviderMark` from `@temps-sdk/ds` for credential identity in
check rows, per-variable check summaries, and provider template choices. The
shared registry currently includes GitHub, GitLab, OpenAI, and Anthropic.

- Accept only a canonical provider ID from backend `automatic_provider` or an
  explicit provider preset. Never infer a brand from variable names, check names,
  arbitrary URLs, or ambiguous detection suggestions. Custom checks without
  confirmed identity use the neutral key icon, even when named after a company.
- Use original official company assets, never generated artwork, traced paths,
  Lucide approximations, or another product's mark (Claude is not Anthropic).
  Preserve geometry, aspect ratio, clear space, and original colors. Do not
  recolor logos to indicate success or failure; retain separate status icons/text.
- Bundle assets locally. Do not request logos from third-party services at runtime.
  Use transparent SVGs in both themes, never raster favicons or opaque backing
  chips. Use the supplied white SVG in dark mode for monochrome marks; a
  currentColor monochrome SVG may invert to white. Keep full-color marks unchanged.
  Use a 24px-high slot (24px wide for symbols; 80px for the Anthropic wordmark)
  and contain the asset without cropping. Keep a text name nearby
  and expose the provider name as image alt text.
- Add a provider only with an official source URL, retrieval date, original-byte
  SHA-256, and light/dark visual verification. Provenance lives beside the shared
  assets in `web/packages/ds/src/credential-provider-assets.ts`. Logos identify
  providers; they do not assert credential validity or company endorsement.

Official sources: [GitHub brand toolkit](https://brand.github.com/foundations/logo),
[GitLab press kit](https://about.gitlab.com/press/press-kit/),
[OpenAI design guidelines](https://openai.com/brand/) and its
[official SVG archive](https://cdn.openai.com/brand/OpenAI-Logos-2025.zip), and
[Anthropic's console](https://console.anthropic.com/) (inline company wordmark).
The company marks remain the property of their respective owners.
