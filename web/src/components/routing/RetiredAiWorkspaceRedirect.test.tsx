// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { retiredAiWorkspaceTarget } from '@/lib/retired-ai-workspace'
import { RetiredAiWorkspaceRedirect } from './RetiredAiWorkspaceRedirect'

installBrowserWindow()
afterEach(cleanup)

test('an old AI workspace link lands on the workspace it named', () => {
  expect(
    retiredAiWorkspaceTarget('?application=app_example&setup=harness')
  ).toBe('/workspaces/app_example')
  expect(retiredAiWorkspaceTarget('?scope=global')).toBe('/workspaces/global')
  expect(retiredAiWorkspaceTarget('')).toBe('/workspaces')
  expect(retiredAiWorkspaceTarget('?setup=workspace&setupStep=2')).toBe(
    '/workspaces'
  )
})

test('an application value that is not an ID never becomes a path', () => {
  for (const application of ['../settings', 'a/b', '', '%2F%2Fexample.com'])
    expect(
      retiredAiWorkspaceTarget(
        `?application=${encodeURIComponent(application)}`
      )
    ).toBe('/workspaces')
})

function Location() {
  const { pathname, search } = useLocation()
  return <p>{`${pathname}${search}`}</p>
}

test('/ai-first redirects instead of rendering the retired page', () => {
  const view = render(
    <MemoryRouter initialEntries={['/ai-first?application=app_example']}>
      <Routes>
        <Route path="/ai-first" element={<RetiredAiWorkspaceRedirect />} />
        <Route path="*" element={<Location />} />
      </Routes>
    </MemoryRouter>
  )
  view.getByText('/workspaces/app_example')
})

test('an old link to a conversation opens that conversation', () => {
  expect(
    retiredAiWorkspaceTarget('?application=app_example&thread=conv_example')
  ).toBe('/chat?chat=conv_example')
  expect(retiredAiWorkspaceTarget('?scope=global&thread=conv_example')).toBe(
    '/chat?chat=conv_example'
  )
})

test('a thread that is not an id falls back to the workspace', () => {
  expect(
    retiredAiWorkspaceTarget('?application=app_example&thread=a%2Fb')
  ).toBe('/workspaces/app_example')
})
