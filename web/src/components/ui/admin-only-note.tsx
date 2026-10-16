// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ShieldAlert } from 'lucide-react'

/**
 * Explains why a write control next to it is disabled.
 *
 * A disabled button on its own tells a restricted user nothing -- they cannot
 * tell a missing permission from a broken page. Render this whenever a control
 * is disabled because the signed-in role lacks the permission, so the page
 * names the gap and who can close it.
 */
export function AdminOnlyNote({
  action,
  role,
}: {
  /** What the user cannot do, e.g. "add users". */
  action: string
  /** The signed-in user's role, named so the message is concrete. */
  role?: string | null
}) {
  return (
    <p
      role="note"
      data-admin-only-note=""
      className="flex items-start gap-2 text-sm text-muted-foreground"
    >
      <ShieldAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <span>
        Only administrators can {action}.
        {role ? ` Your role is "${role}".` : ''} Ask an administrator if you
        need this access.
      </span>
    </p>
  )
}
