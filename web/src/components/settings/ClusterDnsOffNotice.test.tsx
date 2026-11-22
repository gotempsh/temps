// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ClusterDnsOffAlert } from './ClusterDnsOffNotice'

describe('ClusterDnsOffAlert', () => {
  test('says what is missing and links to the Cluster DNS card', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ClusterDnsOffAlert title="Cluster DNS is off">
          Apps on other nodes cannot reach this database.
        </ClusterDnsOffAlert>
      </MemoryRouter>
    )
    expect(html).toContain('Cluster DNS is off')
    expect(html).toContain('Apps on other nodes cannot reach this database.')
    expect(html).toContain('href="/settings/nodes#cluster-dns"')
  })
})
