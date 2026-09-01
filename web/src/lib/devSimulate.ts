// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * TEMPORARY dev-only switch to simulate a brand-new installation — no
 * projects, no Git connections, nothing set up — so we can build and fix the
 * first-run / empty-state experience against a dev instance that actually has
 * seeded data.
 *
 * Gated on `import.meta.env.DEV` so it can NEVER take effect in a real build.
 * Flip `ENABLED` to false (or delete this file and its call sites) when done.
 *
 * Call sites: useActivationSignals.ts, pages/Projects.tsx.
 */
const ENABLED = false

export const SIMULATE_EMPTY_INSTALL = import.meta.env.DEV && ENABLED
