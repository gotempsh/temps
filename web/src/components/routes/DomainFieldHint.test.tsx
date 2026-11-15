// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { DomainFieldHint, type ManagedDomainsStatus } from './DomainFieldHint'

function render(
  status: ManagedDomainsStatus,
  { isLocalMode = false, inputType = 'select' as 'select' | 'manual' } = {}
) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <DomainFieldHint
        isLocalMode={isLocalMode}
        status={status}
        inputType={inputType}
      />
    </MemoryRouter>
  )
}

test('no managed domains links to where one is added', () => {
  const html = render('empty')
  expect(html).toContain('No managed domains yet')
  expect(html).toContain('href="/domains/add"')
})

test('a failed read is not reported as "no managed domains"', () => {
  const html = render('error')
  expect(html).toContain('could not be loaded')
  expect(html).not.toContain('No managed domains yet')
  expect(html).not.toContain('/domains/add')
})

test('a pending read does not claim there are no domains', () => {
  const html = render('pending')
  expect(html).toContain('Checking your managed domains')
  expect(html).not.toContain('No managed domains yet')
})

test('available domains describe the active input mode', () => {
  expect(render('available', { inputType: 'select' })).toContain(
    'Choose from your managed domains'
  )
  expect(render('available', { inputType: 'manual' })).toContain(
    'Enter the full domain name'
  )
})

test('local mode keeps its own explanation whatever the read did', () => {
  expect(render('empty', { isLocalMode: true })).toContain(
    'local development mode'
  )
})
