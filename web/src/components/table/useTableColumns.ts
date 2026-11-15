// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useUrlState } from '@temps-sdk/ds'

export interface TableColumnOption<Key extends string> {
  key: Key
  label: string
  required?: boolean
}

export function normalizeTableColumns<Key extends string>(
  value: unknown,
  options: readonly TableColumnOption<Key>[],
  defaults: readonly Key[]
): Key[] {
  if (
    !Array.isArray(value) ||
    !value.some((key) => options.some((option) => option.key === key))
  ) {
    return [...defaults]
  }
  return options
    .filter((option) => option.required || value.includes(option.key))
    .map((option) => option.key)
}

/** URL overrides are shareable; browser preferences apply when no override exists. */
export function useTableColumns<Key extends string>({
  options,
  defaults,
  storageKey,
}: {
  options: readonly TableColumnOption<Key>[]
  defaults: readonly Key[]
  storageKey: string
}) {
  const { get, patch } = useUrlState()
  const [preferred, setPreferred] = useState(() => {
    try {
      const raw = localStorage.getItem(storageKey)
      return normalizeTableColumns(
        raw ? JSON.parse(raw) : undefined,
        options,
        defaults
      )
    } catch {
      return [...defaults]
    }
  })
  const override = get('columns')
  const visibleKeys =
    override === null
      ? preferred
      : normalizeTableColumns(override.split(','), options, defaults)
  const changeColumns = (keys: readonly Key[]) => {
    const next = normalizeTableColumns(keys, options, defaults)
    setPreferred(next)
    patch({ columns: next.join(',') })
    try {
      localStorage.setItem(storageKey, JSON.stringify(next))
    } catch {
      // The current selection still works when browser storage is unavailable.
    }
  }
  return {
    visibleKeys,
    changeColumns,
    resetColumns: () => changeColumns(defaults),
  }
}
