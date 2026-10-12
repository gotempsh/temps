// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'

import { getPageFlowOptions } from '@/api/client/@tanstack/react-query.gen'
import type { PageFlowEntry, ProjectResponse } from '@/api/client/types.gen'
import { PageFlow } from './PageFlow'

const START = new Date('2026-01-01T00:00:00.000Z')
const END = new Date('2026-01-02T00:00:00.000Z')

function entry(path: string, entries: number, views: number): PageFlowEntry {
  return {
    page_path: path,
    entry_count: entries,
    total_views: views,
    bounce_rate: 0.25,
    bounce_count: 0,
    entry_rate: 0,
    exit_count: 0,
    exit_rate: 0,
    avg_time_on_page: 5,
  }
}

function renderJourney(pages: PageFlowEntry[]) {
  const project = { id: 7, slug: 'demo', name: 'demo' } as ProjectResponse
  const client = new QueryClient()
  const options = getPageFlowOptions({
    query: {
      project_id: project.id,
      environment_id: undefined,
      start_date: START.toISOString(),
      end_date: END.toISOString(),
    },
  })
  client.setQueryData(options.queryKey, {
    total_pages: pages.length,
    total_sessions: 10,
    top_entry_pages: pages,
    top_exit_pages: [],
    drop_off_points: [],
    transitions: [],
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <PageFlow
        project={project}
        startDate={START}
        endDate={END}
        environment={undefined}
      />
    </QueryClientProvider>
  )
}

describe('journey entry pages table', () => {
  const html = renderJourney([
    entry('/docs', 15, 15),
    entry('/pricing', 12, 12),
    entry('/a', 1, 1),
    entry('/blog', 1, 12),
  ])

  test('column headers are buttons with accessible names', () => {
    for (const label of ['Page', 'Entries', 'Views', 'Bounce Rate']) {
      expect(html).toContain(`aria-label="Sort by ${label},`)
    }
    expect(html).toContain('type="button"')
  })

  test('the default sort is Entries, descending, and only that column reports it', () => {
    expect(html.match(/aria-sort="[a-z]+"/g)).toEqual([
      'aria-sort="descending"',
    ])
    expect(html).toMatch(/aria-sort="descending"[^>]*>.*Sort by Entries/s)
    const order = [
      ...html.matchAll(/<span class="cursor-default"[^>]*>([^<]+)</g),
    ]
      .map((match) => match[1])
      // Each path is rendered once per row (the tooltip body is not in the
      // static markup).
      .filter((path, index, all) => all.indexOf(path) === index)
    expect(order).toEqual(['/docs', '/pricing', '/a', '/blog'])
  })
})
