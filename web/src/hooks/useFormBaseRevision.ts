// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'

/**
 * The revision a settings form's values are based on, to send back as
 * `expected_revision` (#2081).
 *
 * While the form is clean it follows `revision`, the one last read. Once the
 * operator edits it, it stays at the revision the edits started from, even
 * when a background refetch brings a newer one: sending that newer revision
 * would let the save overwrite another admin's change to the same field
 * without a conflict. `adopt` moves it explicitly, for a form that merged a
 * re-read into its unsaved edits and showed the operator why.
 */
export function useFormBaseRevision(
  dirty: boolean,
  revision: string | undefined
): { baseRevision: string | undefined; adopt: (revision?: string) => void } {
  const [frozen, setFrozen] = useState(revision)
  useEffect(() => {
    if (!dirty) setFrozen(revision)
  }, [dirty, revision])
  return { baseRevision: dirty ? frozen : revision, adopt: setFrozen }
}
