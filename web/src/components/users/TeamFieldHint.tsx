// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'

/**
 * The note under the Team field of the Create user form. An empty team list
 * is the common state of a new instance; saying only "Optional" leaves the
 * select with a single "No team" entry and no way to tell why.
 */
export function TeamFieldHint({
  status,
}: {
  status: 'pending' | 'error' | 'empty' | 'available'
}) {
  switch (status) {
    case 'error':
      return (
        <>
          Teams could not be loaded. You can still create the user and add them
          from the Teams page later.
        </>
      )
    case 'empty':
      return (
        <>
          No teams exist yet, so the user will not belong to one. Optional: you
          can{' '}
          <Link className="underline underline-offset-4" to="/settings/teams">
            create a team
          </Link>{' '}
          and add this user to it afterwards.
        </>
      )
    case 'pending':
    case 'available':
      return (
        <>
          Optional. Adds the user to the selected team after creating their
          account.
        </>
      )
  }
}
