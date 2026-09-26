// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  labelFiltersToTuples,
  serializeLabelFilters,
  tuplesToLabelFilters,
} from './label-filters'

describe('label filter persistence', () => {
  test('blank draft rows survive locally without entering the URL', () => {
    const rows = [
      { key: ' method ', value: ' GET ' },
      { key: '', value: '' },
    ]
    expect(serializeLabelFilters(rows)).toBe('method=GET')
    expect(labelFiltersToTuples(rows)).toEqual([['method', 'GET']])
    expect(rows).toHaveLength(2)
  })

  test('URL echoes compare equal while an external navigation differs', () => {
    const draft = [
      { key: 'method', value: 'GET' },
      { key: '', value: '' },
    ]
    expect(
      serializeLabelFilters(tuplesToLabelFilters([['method', 'GET']]))
    ).toBe(serializeLabelFilters(draft))
    expect(
      serializeLabelFilters(tuplesToLabelFilters([['method', 'POST']]))
    ).not.toBe(serializeLabelFilters(draft))
  })

  test('absent API filters are empty', () => {
    expect(tuplesToLabelFilters(null)).toEqual([])
    expect(tuplesToLabelFilters(undefined)).toEqual([])
  })
})
