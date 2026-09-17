// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { toast } from 'sonner'

/**
 * A thin wrapper over `sonner`'s `toast`, shaped around RULES.md's
 * Notifications rule: "A toast is state · headline · fact, ≤ 6 words, naming
 * the object. Never on page load, never in a loop." — and only for
 * background events the user isn't currently watching (in-page state like
 * validation/"not set up"/empty belongs in `Callout`/`PageState`;
 * confirmation of the user's own click belongs on the control itself, see
 * `CopyAction`).
 *
 * Deliberately does not export a `Toaster` — the host app keeps mounting
 * `sonner`'s own `<Toaster />` once (unchanged, outside this package's
 * scope, see `App.tsx`).
 *
 * `notify.ok`/`notify.fail` are a headline + optional supporting detail,
 * matching how the app already calls `toast.success`/`toast.error` today
 * (headline string, optional `{ description }`) — this does not change that
 * shape, it just gives it one name so every call site reads the same way.
 * `message` is not truncated or word-counted at runtime (honour-system, like
 * the rest of RULES.md's copy rules) — keep it to the "state · headline ·
 * fact, ≤ 6 words, naming the object" pattern by eye.
 */
export const notify = {
  /** A background action succeeded. `description` is optional supporting detail, not a second headline. */
  ok(message: string, description?: string): void {
    toast.success(message, description ? { description } : undefined)
  },
  /** A background action failed. `description` is optional supporting detail — the actual error, not a repeat of the headline. */
  fail(message: string, description?: string): void {
    toast.error(message, description ? { description } : undefined)
  },
}
