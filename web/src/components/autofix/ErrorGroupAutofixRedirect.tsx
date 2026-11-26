// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { RedirectWithQuery } from '@/components/routing/RedirectWithQuery'

/**
 * `/errors/:errorGroupId/autofix`, the old "Fix with AI" address, now opens
 * the error group, where autofix incidents start (ADR-116 D2).
 */
export function ErrorGroupAutofixRedirect() {
  return <RedirectWithQuery to="../errors/:errorGroupId" />
}
