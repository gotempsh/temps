// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { FacetInfo, FacetStatus } from '@/api/client/types.gen'
import { classifyAttributeKey } from '@/lib/attribute-facets'
import {
  AttributeFilterControls,
  AttributeFilterNotice,
} from './AttributeFilter'

function facet(
  key: string,
  status: FacetStatus = 'completed',
  error_message?: string
): FacetInfo {
  return {
    attribute_key: key,
    backend: 'clickhouse',
    created_at: '2026-01-01T00:00:00Z',
    rows_backfilled: 0,
    slot: 1,
    status,
    error_message,
  }
}

function notice(
  facets: FacetInfo[] | undefined,
  key: string,
  options: {
    facetedOnly?: boolean
    creationBlocker?: string | null
    creating?: boolean
    value?: string
  } = {}
) {
  return renderToStaticMarkup(
    <AttributeFilterNotice
      state={classifyAttributeKey(facets, key, options.value)}
      attrKey={key}
      facetedOnly={options.facetedOnly ?? false}
      hasFacets={(facets ?? []).length > 0}
      creationBlocker={options.creationBlocker ?? null}
      creating={options.creating ?? false}
      onCreate={() => {}}
    />
  )
}

describe('AttributeFilterNotice', () => {
  test('with no facets and no key it explains what facets do instead of vanishing', () => {
    const html = notice([], '')
    expect(html).toContain('Turn an attribute into a facet')
    expect(html).toContain('role="status"')
  })

  test('once facets exist an empty filter stays quiet', () => {
    expect(notice([facet('tier')], '')).toBe('')
  })

  test('nothing is claimed about a key before the facet list arrives', () => {
    expect(notice(undefined, 'tier')).toBe('')
  })

  test('an unfaceted key states the cost and offers the facet', () => {
    const html = notice([facet('tier')], 'http.route')
    expect(html).toContain('&quot;http.route&quot; is not a facet')
    expect(html).toContain('slow on long ranges')
    expect(html).toContain('Create facet')
    expect(html).not.toContain('disabled=""')
  })

  test('where scanning is not allowed it says the filter is not applied', () => {
    const html = notice([facet('tier')], 'http.route', { facetedOnly: true })
    expect(html).toContain('cannot be filtered here')
    expect(html).toContain('every span in the project')
    expect(html).toContain('Create facet')
  })

  test('creation is disabled with the reason when a facet cannot be made', () => {
    const html = notice([facet('tier')], 'bad key', {
      creationBlocker: 'Facet keys start with a letter.',
    })
    expect(html).toContain('disabled=""')
    expect(html).toContain('Facet keys start with a letter.')
  })

  test('creation shows progress and cannot be repeated while it runs', () => {
    const html = notice([], 'http.route', { creating: true })
    expect(html).toContain('animate-spin')
    expect(html).toContain('disabled=""')
  })

  test('a facet still backfilling warns that older spans may be missing', () => {
    const html = notice([facet('tier', 'running')], 'tier')
    expect(html).toContain('still being indexed')
    expect(html).toContain('may be missing')
    expect(html).not.toContain('Create facet')
  })

  test('a completed facet confirms the filter is fast and offers nothing', () => {
    const html = notice([facet('tier')], 'tier')
    expect(html).toContain('indexed facet')
    expect(html).not.toContain('Create facet')
  })

  test('a failed facet shows the server reason', () => {
    const html = notice([facet('tier', 'failed', 'mutation timed out')], 'tier')
    expect(html).toContain(
      'Indexing &quot;tier&quot; failed: mutation timed out'
    )
  })

  test('an unsendable key or value is explained', () => {
    const html = notice([facet('tier')], 'tier', { value: 'a,b' })
    expect(html).toContain('cannot contain &quot;,&quot;')
  })
})

describe('AttributeFilterControls', () => {
  const render = (key: string, keys: string[] = ['tier', 'region']) =>
    renderToStaticMarkup(
      <AttributeFilterControls
        attrKey={key}
        attrValue=""
        facetKeys={keys}
        onKeyChange={() => {}}
        onValueChange={() => {}}
      />
    )

  test('every facet is offered as a suggestion but any key can be typed', () => {
    const html = render('')
    expect(html).toContain('<option value="tier"')
    expect(html).toContain('<option value="region"')
    expect(html).toContain('aria-label="Attribute key"')
  })

  test('the value input appears once a key is entered', () => {
    expect(render('')).not.toContain('aria-label="Value for')
    expect(render(' tier ')).toContain('aria-label="Value for tier"')
  })
})
