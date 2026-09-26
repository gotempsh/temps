// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { CardGrid } from './card-grid'

test('card collections expose full pagination, including on one-page results', () => {
  const markup = renderToStaticMarkup(<CardGrid title="Projects" items={['API']} keyFn={item => item} renderCard={item => item} pagination={{page: 1, pageSize: 10, total: 1, totalPages: 1, onPageChange: () => {}, onPageSizeChange: () => {}, pageSizeOptions: [10, 25]}} />)
  expect(markup).toContain('Showing 1–1 of 1')
  expect(markup).toContain('aria-label="Items per page"')
  expect(markup).toContain('disabled="" aria-label="Next page"')
})
