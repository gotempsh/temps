// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, useLocation, useNavigate } from 'react-router'
import { Activity } from 'lucide-react'
import { SectionNavLayout, type SectionNavLink } from './SectionNavLayout'

import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'

installBrowserWindow()
afterEach(cleanup)

const titles = [
  'Overview',
  'Pages',
  'Funnels',
  'Events',
  'Replays',
  'Visitors',
  'Journey',
  'Globe',
]
const links: SectionNavLink[] = titles.map((title) => ({
  key: title.toLowerCase(),
  title,
  to: `/analytics/${title.toLowerCase()}`,
  icon: Activity,
  active: false,
}))

function Harness() {
  const location = useLocation()
  const navigate = useNavigate()
  return (
    <div>
      <button onClick={() => navigate(-1)}>back</button>
      <output data-testid="path">{location.pathname}</output>
      <SectionNavLayout title="Analytics" links={links}>
        <p>content</p>
      </SectionNavLayout>
      <output data-testid="search">{location.search}</output>
    </div>
  )
}

function finder(view: ReturnType<typeof render>) {
  return view.getAllByLabelText('Find analytics page')[0] as HTMLInputElement
}

test('the page finder keeps fast typing and writes it to the URL with replace', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/analytics/pages?tab=a']}>
      <Harness />
    </MemoryRouter>
  )
  const input = finder(view)
  let typed = ''
  for (const char of 'qa-ui-qj14a') {
    typed += char
    fireEvent.change(input, { target: { value: typed } })
  }
  expect(input.value).toBe('qa-ui-qj14a')
  await waitFor(() => {
    const params = new URLSearchParams(
      view.getByTestId('search').textContent ?? ''
    )
    expect(params.get('find')).toBe('qa-ui-qj14a')
    expect(params.get('tab')).toBe('a')
  })
  expect(input.value).toBe('qa-ui-qj14a')
  view.unmount()
})

test('the page finder is restored from the URL and filters the links', () => {
  const view = render(
    <MemoryRouter initialEntries={['/analytics/pages?find=fun']}>
      <Harness />
    </MemoryRouter>
  )
  expect(finder(view).value).toBe('fun')
  expect(view.getAllByText('Funnels').length).toBeGreaterThan(0)
  expect(view.queryAllByText('Replays')).toHaveLength(0)
  view.unmount()
})

test('following a link and pressing Back restores the page finder', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/analytics/pages?find=fun']}>
      <Harness />
    </MemoryRouter>
  )
  expect(finder(view).value).toBe('fun')
  await act(async () => {
    fireEvent.click(view.getAllByText('Funnels')[0])
  })
  expect(view.getByTestId('path').textContent).toBe('/analytics/funnels')
  expect(view.getByTestId('search').textContent).toBe('')
  await act(async () => {
    fireEvent.click(view.getByText('back'))
  })
  expect(view.getByTestId('path').textContent).toBe('/analytics/pages')
  expect(view.getByTestId('search').textContent).toBe('?find=fun')
  expect(finder(view).value).toBe('fun')
  view.unmount()
})
