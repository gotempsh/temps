// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Detail } from './detail'
import { PageContainer } from '../page-header'

test('embedded detail reuses its parent shell in loading and loaded states', () => {
  for (const title of ['Loading API key', 'Deployment key']) {
    const markup = renderToStaticMarkup(
      <PageContainer>
        <Detail embedded title={title} facts={[]} main={<p>Permissions</p>} />
      </PageContainer>
    )
    expect(markup.match(/data-page-container/g)).toHaveLength(1)
    expect(markup).toContain(title)
  }
})
test('standalone detail retains its page shell', () => {
  const markup = renderToStaticMarkup(
    <Detail title="Repository" facts={[]} main={null} />
  )
  expect(markup.match(/data-page-container/g)).toHaveLength(1)
})

test('detail main uses the full width unless an aside is present', () => {
  const full = renderToStaticMarkup(
    <Detail title="Variable" facts={[]} main={<p>History</p>} />
  )
  expect(full).not.toContain('lg:grid-cols-3')
  expect(full).not.toContain('lg:col-span-2')
  const split = renderToStaticMarkup(
    <Detail
      title="Variable"
      facts={[]}
      main={<p>History</p>}
      aside={<p>Metadata</p>}
    />
  )
  expect(split).toContain('lg:grid-cols-3')
  expect(split).toContain('lg:col-span-2')
})
