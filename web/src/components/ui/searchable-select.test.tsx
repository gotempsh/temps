// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { fieldValueName } from './accessible-name'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from './searchable-select'

installBrowserWindow()
afterEach(cleanup)

const OPTIONS: SearchableSelectOption[] = [
  { value: 'all', label: 'All projects' },
  { value: 'web', label: 'Web app' },
]

test('the name keeps the visible value and adds the field label', () => {
  expect(fieldValueName('Project', 'All projects')).toBe(
    'Project: All projects'
  )
  expect(fieldValueName(undefined, 'All projects')).toBe('All projects')
})

// A combobox button takes its name from the author, not from its text.
test('a combobox with no label is still named by what it shows', async () => {
  const view = render(
    <SearchableSelect value="web" onValueChange={() => {}} options={OPTIONS} />
  )
  const trigger = await view.findByRole('combobox')
  expect(trigger.getAttribute('aria-label')).toBe('Web app')
})

test('the placeholder names an empty selection', async () => {
  const view = render(
    <SearchableSelect
      onValueChange={() => {}}
      options={OPTIONS}
      placeholder="Filter by project"
    />
  )
  const trigger = await view.findByRole('combobox')
  expect(trigger.getAttribute('aria-label')).toBe('Filter by project')
})

test('a label prop is prefixed to the visible value', async () => {
  const view = render(
    <SearchableSelect
      label="Project"
      value="all"
      onValueChange={() => {}}
      options={OPTIONS}
    />
  )
  const trigger = await view.findByRole('combobox')
  const name = trigger.getAttribute('aria-label')
  expect(name).toBe('Project: All projects')
  // WCAG 2.5.3: the visible text is part of the accessible name.
  expect(name).toContain(trigger.textContent?.trim() ?? '')
})
