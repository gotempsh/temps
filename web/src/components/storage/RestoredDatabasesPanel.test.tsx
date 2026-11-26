// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { report } from '@/lib/restored-databases.fixture'
import { RestoredDatabasesView } from './RestoredDatabasesPanel'

const render = (props: Partial<Parameters<typeof RestoredDatabasesView>[0]>) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <RestoredDatabasesView report={report} onUse={() => {}} {...props} />
    </MemoryRouter>
  )

describe('RestoredDatabasesView', () => {
  it('names the project that misses its data and offers the restored one', () => {
    const html = render({})
    expect(html).toContain('Restored data no linked app reads')
    expect(html).toContain(
      '(production, staging) reads shop_production and shop_staging'
    )
    expect(html).toContain('title="Point Shop at shop_prod"')
    expect(html).toContain('>shop_prod</span>')
    expect(html).toContain('href="/projects/shop"')
  })

  it('shows only the summary when every restored database is read', () => {
    const html = render({ report: { ...report, missing: [] } })
    expect(html).not.toContain('Restored data no linked app reads')
    expect(html).toContain('Last restore brought back 4 databases and 1 user')
  })

  it('explains what to do when no restored database can be a link name', () => {
    const html = render({
      report: {
        ...report,
        databases: report.databases.filter((d) => d.name !== 'shop_prod'),
      },
    })
    expect(html).toContain('Rename a restored database')
    expect(html).not.toContain('>Use</button>')
  })

  it('locks every action while a link is being changed', () => {
    const html = render({ pendingProjectId: 1 })
    expect(html).toMatch(
      /<button[^>]*disabled[^>]*title="Point Shop at shop_prod"/
    )
  })
})
