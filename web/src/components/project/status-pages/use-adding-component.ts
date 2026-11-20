// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useIsMutating, type MutationKey } from '@tanstack/react-query'
import { useId, useMemo } from 'react'

/** What a form shares with its `StatusPageComponentsEditor`. */
export interface ComponentAddition {
  /** Tags the editor's lookups, so only this form counts them. */
  mutationKey: MutationKey
  /**
   * Whether a service chosen in this form is still being added. Its
   * environments are loading and the row is not in the form yet, so a save
   * now would leave it out: the form holds its save until it lands.
   */
  adding: boolean
}

/**
 * The pending addition of one form, scoped to it: a lookup left running by
 * another status page's editor never holds this form's save.
 */
export function useComponentAddition(): ComponentAddition {
  const id = useId()
  const mutationKey = useMemo(() => ['status-page-add-component', id], [id])
  const adding = useIsMutating({ mutationKey, exact: true }) > 0
  return { mutationKey, adding }
}
