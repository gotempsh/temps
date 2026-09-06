// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Operator component library ("op").
 *
 * The primitives themselves now live in the `@temps-sdk/op` package
 * (temps/web/packages/op) so the console and the sandbox render the exact
 * same components. This file stays as a thin re-export so the ~30
 * `@/components/op` imports across the sandbox keep resolving.
 *
 * See docs/design-system-handoff.md §6 and docs/brand-guidelines.md §6.
 */
export * from '@temps-sdk/op'
