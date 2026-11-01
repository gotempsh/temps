// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { PageNamedByChromeContext, PageTitleLevelContext } from './page-context'
import { PageContainer, PageHeader } from './page-header'

test('a page container is full width unless asked otherwise', () => {
  const html = renderToStaticMarkup(<PageContainer>Body</PageContainer>)
  expect(html).toContain('data-page-width="full"')
  expect(html).not.toContain('max-w-')
})

test('a form page is a capped column aligned with the bar above it', () => {
  const form = renderToStaticMarkup(
    <PageContainer width="form">Body</PageContainer>
  )
  expect(form).toContain('data-page-width="form"')
  expect(form).toContain('max-w-5xl')
  expect(form).not.toContain('mx-auto')
})

test('a page container inside another adds no second gutter or cap', () => {
  const html = renderToStaticMarkup(
    <PageContainer width="form">
      <PageContainer width="full">Body</PageContainer>
    </PageContainer>
  )
  expect(html.match(/data-page-container[=\s>]/g)?.length).toBe(1)
  expect(html).toContain('data-page-container-nested')
  expect(html.match(/px-4 py-6/g)?.length).toBe(1)
})

test('a title the chrome already shows is kept for screen readers only', () => {
  const html = renderToStaticMarkup(
    <PageNamedByChromeContext.Provider value={true}>
      <PageHeader title="Metrics" description="Explore metrics." />
    </PageNamedByChromeContext.Provider>
  )
  expect(html).toContain('<h1 class="sr-only">Metrics</h1>')
  expect(html).toContain('Explore metrics.')
})

test('section headings are never hidden by the chrome', () => {
  const html = renderToStaticMarkup(
    <PageNamedByChromeContext.Provider value={true}>
      <PageHeader headingLevel="h2" title="Builds" />
    </PageNamedByChromeContext.Provider>
  )
  expect(html).toContain('<h2 class="text-2xl font-semibold tracking-tight">')
})

test('inside a frame that has the top heading, a page title is an h2', () => {
  const html = renderToStaticMarkup(
    <PageTitleLevelContext.Provider value="h2">
      <PageNamedByChromeContext.Provider value={true}>
        <PageHeader title="Metrics" />
      </PageNamedByChromeContext.Provider>
    </PageTitleLevelContext.Provider>
  )
  // Still treated as the page's title: the tab strip names it, so it is
  // kept for screen readers only.
  expect(html).toBe('<h2 class="sr-only">Metrics</h2>')
})

test('outside a frame a page title stays an h1', () => {
  const html = renderToStaticMarkup(<PageHeader title="Projects" />)
  expect(html).toContain('<h1 class="text-2xl font-semibold tracking-tight">')
})
