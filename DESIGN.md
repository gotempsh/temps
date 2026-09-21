# Temps console design standard

This is the authoritative UI reference for existing contributions to
`web/src`. Use the existing shadcn/ui components, Tailwind semantic tokens,
and shared layouts. The separate "operator ink" prototype app has been
retired; it is not an alternative design direction and never was one worth
resurrecting.

`web/packages/ds` (`@temps-sdk/ds`) is a real, maintained package — the
codification of these same conventions into reusable page/record/list/
settings templates, built on `@temps-sdk/ui`. See its
`docs/design-system-handoff.md` and `docs/RULES.md`. It is not yet a mandate
to migrate `web/src` screen by screen — that migration is tracked as numbered
follow-ups in the handoff doc, not assumed to have happened. New work that
fits one of its templates should use it; retrofitting an existing screen is a
deliberate, separate decision, not an incidental side effect of an unrelated
change.

The product should feel like one application: consistent page width, compact
resource lists, predictable controls, and useful states when data is absent.
These rules describe the target standard, not a claim that every old screen
already follows it. When changing a screen, bring its affected layout and states
into line. Do not copy a legacy inconsistency into new code.

## 1. Page layout: full width, one padding owner

Every sidebar destination uses the available content width, including settings,
users, teams, API keys, notifications, and their create/edit/detail routes.

- Use [PageContainer and PageHeader](web/src/components/layout/PageContainer.tsx).
  `PageContainer` is always full width; it has no width variant.
- The route layout owns page gutters: `px-4 py-6 sm:px-6 lg:px-8`.
  Do not repeat those gutters inside the page.
- Settings routes inherit the container from their parent layout. Their root
  should be `w-full min-w-0 space-y-6`, not another `PageContainer`.
- Do not center sidebar pages inside `container`, `max-w-7xl`, `max-w-3xl`,
  or `mx-auto`. Do not reintroduce narrow page-shell variants.
- Constrain explanatory text where it improves reading, not the entire page,
  form, table, or resource surface.
- Align the heading, toolbar, collection, and footer to the same content edges.
  Use `min-w-0` for flex/grid children containing long names or URLs.
- Use `space-y-6` between major sections, `gap-4` within groups, and
  `gap-2` between related controls.

A page header has one `h1`, a short description, and its primary action.
Use `PageHeader` instead of hand-written variants. The title is
`text-2xl font-semibold tracking-tight`; the description is
`text-sm text-muted-foreground`. Actions wrap below the title on small screens.
No large hero, oversized title, or decorative introduction before useful content.

## 2. Canonical resource-list structure

Use the Teams page's structural approach, with a **compact** empty state.
Do not copy its oversized empty panel or Authentication's nested dashed box.

The order is:

1. Page heading, description, and primary create/add action.
2. Search, filters, and optional bulk actions when applicable.
3. One full-width, solid-bordered collection surface.
4. Shared pagination when needed.

The surface is `rounded-lg border bg-card text-card-foreground`. It has no
decorative shadow. A shared `Card` may be used with `shadow-none`; do not
put another bordered card or dashed empty box inside it. Use dividers between
rows and sections rather than nesting surfaces.

The same collection surface holds loading, empty, error, or populated content.
An empty collection must not become a separate page design.

## 3. Collection states

Never infer "empty" from a failed or still-loading request.

| State | Content | Action |
| --- | --- | --- |
| Initial loading | Skeletons shaped like the expected list/table | Keep the page heading visible |
| No records yet | Compact icon, specific title, short explanation | Create/add if permitted |
| No filter matches | "No matching …", explain the active filters | Clear filters; do not imply there are no records |
| Request failed | Contextual error in the collection surface | Retry; retain filters and useful existing data |
| Feature not configured | Name the missing dependency and explain the outcome | Link directly to setup |
| Records available | Shared table or an explicitly justified card collection | Row actions and pagination |

Background refreshes must not replace usable rows with an empty state.
Keep existing data visible and communicate the refresh or refresh failure
without losing the user's position.

Unconfigured features remain discoverable. A permission restriction changes
the available action and explanation; it does not turn a failed request into
an empty collection.

### Compact empty state

Use [EmptyState](web/src/components/ui/empty-state.tsx) with
`size="compact"` explicitly. Its compact variant uses a 240px minimum height,
a small muted icon circle, a base-size title, and a short muted description.
The height may grow for wrapped content; it is not a fixed-height clipping box.

```tsx
import { Users } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'

<div className="rounded-lg border bg-card text-card-foreground">
  <EmptyState
    size="compact"
    icon={Users}
    title="No teams yet"
    description="Create a team to organize people and grant project access."
    action={<Button onClick={openCreateTeam}>Create team</Button>}
  />
</div>
```

- The page header's create action may remain visible; the empty-state action
  invokes the same flow. Do not show a disabled administrative action to a
  user who cannot perform it.
- Prefer specific nouns: "No teams yet", not "Nothing here".
- Keep descriptions to one or two short sentences. Never use an error as
  onboarding copy.
- Do not add a second border, shadow, dashed inset, oversized illustration,
  or `min-h-[400px]` to a resource empty state.
- The component's existing `size="default"` is a legacy large variant, not
  the resource-list standard. Do not omit `size="compact"`.
- Do not add new uses of `EmptyPlaceholder` or recreate empty-state markup
  inline. Migrate existing uses when working on that state.

## 4. Populated tables and lists

Use [Table](web/src/components/ui/table.tsx) for collections whose records
share comparable fields: users, teams, keys, routes, providers, and similar
administrative resources. Do not invent a different row component per page.

- Use `TableHeader`, `TableBody`, `TableRow`, `TableHead`, and
  `TableCell`. Preserve their shared padding and typography.
- The first meaningful column identifies the resource. Make its name a real
  [RecordLink](web/packages/ds/src/record-link.tsx) when a detail page exists.
  It is visibly underlined with a persistent right arrow. Secondary metadata is muted.
- Put status in a text-labeled badge; color alone never carries the meaning.
- Right-align row actions in the last column. Give icon-only actions an
  accessible name that identifies the resource.
- Keep links, menus, and selection independently operable. Table rows never
  navigate: no row `onClick`, link overlay, or `role="button"`.
- Use monospace only for identifiers, hashes, commands, and code. Truncate
  long values intentionally, with a way to inspect or copy the full value.
- Sorting uses a real button in the header and exposes its sort state.
- Selection uses the shared Checkbox, including indeterminate select-all.
  Bulk actions must state their scope.
- Keep column alignment stable during loading. Do not use fake records as
  skeleton content.


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

### Resource-detail navigation decision

Use **one `RecordLink` in the identity column** for a record's primary detail
route. Its name is underlined and its arrow remains visible before hover, on
touch screens, and in both themes. This is the canonical detail affordance;
do not alternate between plain text links, a separate "View" button, and
clickable rows. The record name is the link text; use `aria-label` when context
is needed (for example, "View GITHUB_TOKEN details").

Navigation is a real router link: Enter, modifier-click, middle-click, copy link,
and open in a new tab work normally. Clicking row whitespace does nothing.
Checkboxes, copy/reveal, status popovers, menus, Edit, and Delete remain separate
controls; they never open details. Do not add a second primary-detail link in
the Actions column. Links to related resources are allowed in their own columns.
Rows without a detail route render plain identity text, without an arrow.

Details and their configuration have stable URLs, not modal-only entry points.
Their facts, checks, forms, and history fill the parent content width. A detail
layout reserves an aside column only when an aside is actually present; never
cap the whole page with `max-w-*`. Paragraphs may have a reading-width limit.

Enforcement: `@temps-sdk/ds` exports `RecordLink`; `DataTable` and `Ledger` do
not expose row-click navigation. The sandbox `/ledger` and the environment
variables table are reference implementations. Existing screens are migrated
when their tables are touched; this decision does not claim all legacy tables
have already been converted. Preserve row expansion/selection as explicit,
separately labeled controls during migration.

The shared Table already owns a horizontal overflow wrapper. On narrow screens,
scroll the table rather than the entire page. Keep identity and primary actions
reachable; if columns are hidden, essential information must remain available
through a detail view or an accessible compact presentation.

Cards are appropriate for discovery surfaces such as the plugin catalog, where
logos, descriptions, and categories help people choose. They are not a second
default for administrative lists. Use a responsive grid with consistent card
anatomy and the same state rules above.

### Pagination

Use [ResponsivePagination](web/src/components/ui/responsive-pagination.tsx),
not page-local previous/next markup. Pass the actual `page`, `pageSize`,
`total`, `totalPages`, and `onPageChange`; supply `onPageSizeChange`
when page size is configurable.

Mobile uses the shared stable Previous / current page / Next row. Desktop
shows the fuller summary and controls. Reset or clamp the page when filters,
page size, or record deletion make the current page invalid. Never show a
negative range or an empty later page as "No records yet".

## 5. Controls and forms

Use existing components from `web/src/components/ui`. Do not replace them
with native lookalikes or locally styled forks.

- **Buttons:** primary for the main action, outline for secondary actions,
  ghost for low-emphasis tools, destructive for destructive actions. Prefer
  shared sizes; do not give each page different control heights.
- **Checkboxes:** use [Checkbox](web/src/components/ui/checkbox.tsx) for
  selection and explicit consent. Never hand-author `input type="checkbox"`
  or pass that type to `Input`. Radix's internal hidden form input is expected.
  For a simple boolean, use `onCheckedChange={(value) => setValue(value === true)}`.
  Preserve disabled states, labels, and form names.
- **Switches:** use the shared Switch for an on/off setting that takes effect
  immediately, not for consent or selecting table rows.
- **Labels:** associate every control with visible text using `id` and
  `htmlFor`, or a correctly wrapping label. A placeholder is not a label.
- **Forms:** use existing React Hook Form, Zod, and shared form patterns.
  Put validation next to the field; retain user input after failures.
- **Async actions:** show progress and prevent duplicate submission. Keep
  readable contrast while pending; communicate the result and recovery action.
- **Destructive actions:** require confirmation that identifies the target
  and consequence. Never style unrelated actions as destructive.
- **Dialogs and menus:** use the shared primitives for focus management,
  keyboard behavior, escape, and focus restoration. Avoid custom overlays.
- **Shortcuts:** use shared create-action conventions where applicable.
  A shortcut is an accelerator, never the only way to reach a feature.

## 6. Typography, color, and surfaces

Use the fonts and tokens defined in [globals.css](web/src/globals.css).
Do not load a different font for an individual console page.

- Page title: `text-2xl font-semibold tracking-tight`.
- Section title: `text-lg font-semibold`.
- Resource labels and table content: `text-sm`; use medium weight for identity.
- Supporting copy: `text-sm text-muted-foreground`.
- Small metadata: `text-xs text-muted-foreground`, never the only place for
  information needed to complete an action.
- Use semantic pairs: `bg-background text-foreground`,
  `bg-card text-card-foreground`, `bg-primary text-primary-foreground`,
  and `border-border`.
- No page-local hardcoded white/black backgrounds, arbitrary gray palettes,
  gradients, or decorative status colors.
- Reuse existing status variants. Combine a label and, where useful, an icon
  with the color. Healthy, pending, warning, and failure remain distinguishable
  without color.
- Use Lucide for interface icons. Official product/provider logos are the
  exception; do not substitute random emoji for them.
- Resource surfaces have a solid border and no large shadow. Elevation belongs
  to overlays such as dialogs and menus, not every settings section.

Hover, selected, focus, disabled, and pending are different states. Define
foreground and background together: an outlined control must not retain white
selected text after its hover background becomes light.

## 7. Responsive, accessible, and functional

- Check at 390px, 768px, and a wide desktop width. Full width means the available
  content area after the sidebar, not the entire viewport.
- Verify both light and dark mode, including hover, selected, pending, and
  disabled controls.
- No document-level horizontal overflow, clipped actions, or inaccessible
  fixed-width forms. Let toolbars wrap and long labels break sensibly.
- Keep visible focus indicators and a logical keyboard order. Test checkbox
  labels and Space, menus, dialogs, and table links.
- Preserve heading hierarchy: one page `h1`, then section headings.
- Honor reduced motion. Use subtle shared transitions, not decorative entrance
  animations for every row.
- Every rendered control must work. Links go to real destinations; search and
  filters change results; retries perform the failed operation again.
- Use query/mutation state for server interactions. Do not duplicate it with
  manual loading flags or conditionally remount stateful dialogs on every update.
- Errors identify what failed and offer a concrete next step. Do not expose
  raw secrets or credentials in error details, previews, or examples.

## 8. Review checklist

Before handing off a changed screen:

- [ ] Full-width layout, one padding owner, shared page header.
- [ ] Primary content appears promptly below the heading.
- [ ] One collection surface; no nested dashed empty panel or decorative shadow.
- [ ] Compact EmptyState explicitly selected; loading, empty, filtered, error,
      and unconfigured states are distinguished.
- [ ] Shared table and pagination, or a justified discovery-card layout.
- [ ] Shared Checkbox/Switch and properly associated labels; no native lookalikes.
- [ ] Permission-aware actions, useful failures, and working retry/setup links.
- [ ] Keyboard, mobile, wide-screen, light, and dark behavior checked.
- [ ] Regression tests cover the changed behavior and important failure states.
- [ ] Documentation and shared components agree, or the remaining legacy gap
      is explicitly identified rather than presented as complete.

Run relevant tests from `web/`: `bun run test`, `bunx tsc --noEmit`,
and the affected Playwright specs via `bun run e2e`. A documentation-only
change needs reference and consistency checks; it does not prove that old
screens have been migrated.

Exceptions require an explicit product decision. Update this document alongside
the shared implementation rather than creating a competing design guide.

## 9. Embedded plugin workspaces

Plugins inherit the resolved Temps light/dark theme and follow live changes. They do not expose an independent theme toggle or persist a separate theme preference. Standalone previews may follow the OS preference.

The host owns plugin identity. Inside the plugin, use a compact view title rather than repeating the plugin name and description. Multi-view plugins use an internal sidebar with Lucide icons and text labels; on narrow screens it becomes a horizontally scrollable navigation strip.

Separate history lists from record details using addressable routes. Paginate history and result collections with shared responsive pagination, clamp pages after filtering or deletion, and preserve keyboard navigation. Starting a job opens its detail view; returning to history must not cancel it.

Plugin workspaces fill the available iframe height. Navigation and the view toolbar remain visible while the content region scrolls. Do not stack the creation form, entire history, and expanded report on one page. Bound expanded diagnostics so long referrer lists do not push every other control out of reach.


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
  Use a fixed 24px slot and contain the asset without cropping. The white backing
  preserves the provided marks in light and dark themes. Keep a text name nearby
  and expose the provider name as image alt text.
- Add a provider only with an official source URL, retrieval date, original-byte
  SHA-256, and light/dark visual verification. Provenance lives beside the shared
  assets in `web/packages/ds/src/credential-provider-assets.ts`. Logos identify
  providers; they do not assert credential validity or company endorsement.

Official sources: [GitHub brand toolkit](https://brand.github.com/foundations/logo),
[GitLab press kit](https://about.gitlab.com/press/press-kit/),
[OpenAI design guidelines](https://openai.com/brand/) and its
[developer-site mark](https://developers.openai.com/favicon.png), and
[Anthropic's company site](https://www.anthropic.com/) (official favicon).
The company marks remain the property of their respective owners.
