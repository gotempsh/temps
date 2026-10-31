# web/AGENTS.md

Rules for the Temps console in `web/`. They add to the repository-wide rules in
[`../AGENTS.md`](../AGENTS.md), which still apply (tests in the same PR, SPDX
headers, Conventional Commits with sign-off, no real third-party names).

React + TypeScript, TanStack Query, shadcn/ui, Tailwind CSS, Rsbuild; the package
manager is `bun`.

- **Design**: root [`DESIGN.md`](../DESIGN.md) is authoritative for existing console
  UI: full-width sidebar pages with one `PageContainer` padding owner (no centred
  max-width wrappers), compact empty states, shared tables and pagination, shadcn
  controls. `web/packages/ds` (`@temps-sdk/ds`) codifies it into page, record,
  list and settings templates and a status vocabulary
  (`web/packages/ds/docs/RULES.md`); migrating `web/src` onto it is tracked in
  `web/packages/ds/docs/design-system-handoff.md`, not assumed. The
  [`temps-design-system`](../.agents/skills/temps-design-system/SKILL.md) skill says
  which applies. The "operator ink" prototype is retired and is no reference.
- **React**: no IIFEs in JSX (extract a helper or component); no hooks after an
  early return; no conditional mounting of stateful components (use `open` props).
- **State**: React Query's `isPending`/`isLoading`/`isError`, never a `useState`
  loading flag; React Hook Form + Zod for every form; invalidate queries after
  mutations.
- **UI**: visual feedback (toast, loading, error) for every user action;
  `CopyButton` for copy-to-clipboard; cards over dropdowns for selections where
  practical; `Skeleton` placeholders matching the real layout for content
  loading, never centred `Loader2` spinners (inline spinners on mutation buttons
  are fine).
- **Mobile**: tables use the shared overflow wrapper (hide a column only if a
  detail view still shows it); filter bars `flex flex-col gap-2 sm:flex-row
  sm:flex-wrap` with `w-full sm:w-[Npx]` selects; grids `grid-cols-1
  md:grid-cols-2 lg:grid-cols-3` (stat cards `grid-cols-2 md:grid-cols-4`); side
  panels `flex-col lg:flex-row` with `w-full lg:w-[Npx]`; the shared `PageHeader`
  (`web/src/components/layout/PageContainer.tsx`); icon-button labels
  `max-sm:sr-only`, never `hidden sm:inline`; `min-w-[Npx]` on scrollable
  containers.
- **Pagination** is the shared `ResponsivePagination`
  (`web/src/components/ui/responsive-pagination.tsx`), never rebuilt per call
  site: below `sm` one row with labelled Previous and Next around
  `{page} / {totalPages}`, no page-size, first/last or direct-page controls; at
  `sm` and up, "Showing X–Y of Z" and the advanced controls.
- **API calls** use the generated SDK in `web/src/api/client/`. Never write a
  `fetch` shim under `web/src/lib/` (one once silently dropped a new field on
  PATCH). A missing binding means the handler lacks `#[utoipa::path]` or `ApiDoc`
  registration: fix it there, and ask for help rather than adding a shim.
- Running Playwright or deployment tests needs credentials from a maintainer;
  none are stored in the repository.

- Tests are Bun tests beside the source they cover (`name.test.ts`/`.test.tsx` in
  the same directory); never import `bun:test` into a production module.
- After a backend API change, regenerate the SDK with `bun run openapi-ts` against
  a restarted server ([`../AGENTS.md` → OpenAPI clients](../AGENTS.md#openapi-clients)).
