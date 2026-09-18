# Design an embedded Temps interface

## Choose the real design source

Follow root `DESIGN.md`, shared `web/src/components/ui/` primitives and
`web/src/globals.css` from the Temps revision being targeted. If the user supplies a
specific design worktree/version, inspect and use that source instead, documenting its
revision. Do not substitute a new visual identity or choose a package based on its name.

As of the 2026-09-18 source review, `@temps-sdk/ds` is retained for existing operator-UI
consumers; its README explicitly says it is **not** the current console design standard.
Its old prototype and `.operator.ink.v1` skin are not equivalent to current console UI.
Re-check that README before future integrations. Do not present an archived prototype or
unpublished workspace export as the standard for all new plugins.

Use a published standalone component package only after verifying its exports, styles,
peer dependencies and compatibility. Where console components are not independently
published, copy the necessary licensed components/tokens as an attributed source snapshot,
record upstream paths and revision, and adapt imports for the standalone build. Avoid
runtime/build paths to a developer's worktree. Keep React deduplicated and include the
component source in Tailwind scanning. Do not rebuild Radix controls as styled native
lookalikes just to avoid integrating their dependencies.

### Standalone integration recipe

1. Select only used primitives from the chosen revision (for example Button, Input,
   Checkbox, Table, Dialog, PageContainer) and recursively inspect their imports. Copy
   their local helpers and preserve notices; rewrite console aliases into plugin-local
   imports. Do not copy authentication contexts or console API clients into the UI kit.
2. Pin their actual React/Radix/Lucide and utility dependencies. Copy the theme variables
   and required Tailwind mappings from the same revision; include its CSS plugins if used.
3. Import Tailwind before the local theme CSS, add an explicit `@source` for the copied
   component directory when outside automatic scanning, and deduplicate React in Vite.
   Self-host the selected font or keep its documented fallback; do not silently load a
   different design-system font.
4. Add a provenance file with upstream commit, paths, license and import-only adaptations.
   Build from a clean checkout and inspect button, checkbox, table and dialog states in
   both themes before designing the rest of the screen.
5. Inspect the host's plugin iframe wrapper and SDK for an actual theme API. If none exists,
   use `prefers-color-scheme` plus an explicit plugin toggle. If a message bridge exists,
   match its schema and validate sender origin/source. Test host theme changes through the
   real iframe before claiming automatic synchronization.

## Screen design

Start with the primary user outcome. Choose a collection, record/detail, or settings
layout. Use the shared page container/header, semantic colors, typography and controls:

- A visible navigation entry and clear page title; no giant marketing hero inside the console.
- Shared tables for comparable records, local horizontal overflow, and responsive actions.
- Text-labeled status badges; color must not be the only status signal.
- Labels tied to controls, keyboard focus, pending states that prevent duplicate actions,
  recoverable errors, and confirmation identifying the target of destructive operations.
- Distinct loading, empty, filtered-empty, error, and unconfigured states. Show missing
  optional dependencies with a concrete benefit and a direct settings link.
- Settings persisted by the plugin API; neither hardcoded demo data nor browser-only state
  should pretend to configure the running plugin.

Use the shared light/dark tokens and test controls, dialogs and portalled content in both
themes. An iframe does not inherit the host document's CSS; include required styles/assets.
Use a documented theme bridge if available, or a clear local/system-theme fallback, without
claiming automatic host synchronization. Never trust arbitrary cross-origin messages as
configuration.

## Asset delivery and navigation

Embed the built UI into the native binary using the SDK's embedded-asset API. Test the
actual `/api/x/<plugin-name>/ui/` path, relative assets, API base paths and nested navigation.
Avoid frontend requests accidentally targeting the Temps root instead of the plugin proxy.
Do not register two competing UI handlers. Use the current template's sidebar nav and UI
manifest as the starting point; verify the plugin appears and opens after installation.

The source installer compiles the entrypoint directly; it does not run an arbitrary Vite
build script first. Generated assets imported by that entrypoint must exist in a clean
checkout. Build and commit a deterministic embedded asset module or the required static
bundle, then verify regeneration has no diff in CI. Never include credentials, private
records or local runtime files in that generated bundle. See the publishing reference.

## Review with real states

Exercise the main workflow and failure paths at 390px, 768px and a wide desktop width.
Check document overflow, long URLs/identifiers, empty results, permission denial, saved
settings and keyboard operation. Confirm dialog focus and dismissal without deleting user
data. Capture catalog screenshots of actual plugin UI in light/dark themes using fixture
or non-sensitive data. Store them in the plugin repository and declare descriptive alt text;
mockups do not prove the UI works.
