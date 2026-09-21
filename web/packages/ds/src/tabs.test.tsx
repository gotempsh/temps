// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@temps-sdk/ui'

test('shared tabs preserve panel semantics, zero counts, and disabled states', () => {
  const markup = renderToStaticMarkup(
    <Tabs defaultValue="checks">
      <TabsList aria-label="Record views">
        <TabsTrigger value="checks" count={0}>
          Checks
        </TabsTrigger>
        <TabsTrigger value="history" disabled>
          History
        </TabsTrigger>
      </TabsList>
      <TabsContent value="checks">Check results</TabsContent>
    </Tabs>
  )
  expect(markup).toContain('role="tablist"')
  expect(markup).toContain('role="tabpanel"')
  expect(markup).toContain('aria-selected="true"')
  expect(markup).toContain('disabled=""')
  expect(markup).toContain('>0</span>')
  expect(markup).toContain('data-[state=active]:border-foreground')
  expect(markup).not.toContain('shadow-sm')
})
