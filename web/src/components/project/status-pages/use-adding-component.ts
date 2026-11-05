// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useIsMutating } from '@tanstack/react-query'

/** Identifies the lookup that adds a component to a status page. */
export const ADD_COMPONENT_KEY = ['status-page-add-component']

/**
 * Whether a component is being added. Its environments are still loading and
 * the row is not in the form yet, so a save now would leave it out: the forms
 * that host the editor hold their save until it lands.
 */
export function useAddingComponent(): boolean {
  return useIsMutating({ mutationKey: ADD_COMPONENT_KEY }) > 0
}
