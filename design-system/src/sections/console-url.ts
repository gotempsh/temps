// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The URL-state hooks moved into `@temps-sdk/ds` (`src/url-state.ts`) so the
 * console, the sandbox and a plugin UI all keep the view in the address the
 * same way. This file stays as a one-line re-export so the fifteen screens
 * that import `./console-url` keep working; new code imports from the package.
 */
export { useUrlState, useUrlNumber, useUrlPatch, useUrlWindow, useUrlSort, useUrlText, forNewView, VIEW_KEYS, KEPT_ON_NAVIGATION,
  type ViewKey, type UrlWindow, type UrlSort } from '@temps-sdk/ds'
