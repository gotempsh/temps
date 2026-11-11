// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { ServiceTlsStatusResponse } from '@/api/client/types.gen'
import { VERIFY_FULL_EXAMPLE } from '@/lib/service-tls'
import { ServiceTlsView } from './ServiceTlsCard'

const noop = () => {}

const offStatus: ServiceTlsStatusResponse = {
  service_id: 7,
  mode: 'off',
  supported: true,
  unsupported_reason: null,
  accepts_plaintext: true,
  active: false,
  pending_restart: false,
  change_restarts_database: true,
  members: [],
  certificate: null,
  certificate_authority: null,
  ca_certificate_path: null,
}

const availableStatus: ServiceTlsStatusResponse = {
  ...offStatus,
  mode: 'available',
  active: true,
  certificate: {
    subject_alt_names: ['postgres-orders', 'localhost', '127.0.0.1'],
    not_before: '2026-11-10T00:00:00Z',
    not_after: '2027-11-10T00:00:00Z',
    fingerprint_sha256: 'aabb',
  },
  certificate_authority: {
    fingerprint_sha256: 'abcdef01',
    not_before: '2026-11-10T00:00:00Z',
    not_after: '2036-11-08T00:00:00Z',
  },
  ca_certificate_path: '/external-services/7/tls/ca-certificate',
}

const managedNode = {
  container_name: 'postgres-orders-1',
  status: 'running',
  active: true,
  self_signed: false,
  pending: false,
}

const clusterStatus: ServiceTlsStatusResponse = {
  ...availableStatus,
  change_restarts_database: false,
  members: [
    managedNode,
    { ...managedNode, container_name: 'postgres-orders-2' },
  ],
}

function render(status: ServiceTlsStatusResponse, applying = false) {
  return renderToStaticMarkup(
    <ServiceTlsView
      serviceName="orders"
      status={status}
      applyingMode={applying ? 'available' : null}
      onChooseMode={noop}
      onDownloadCa={noop}
    />
  )
}

describe('ServiceTlsView', () => {
  it('offers Off and Available but not Required yet', () => {
    const html = render(offStatus)
    expect(html).toContain('aria-pressed="true"')
    expect(html).toContain('Not supported yet')
    // Required is the only disabled mode on a supported, idle service.
    expect(html.match(/disabled=""/g)?.length).toBe(1)
    expect(html).toContain('created the first time TLS is turned on')
    expect(html).not.toContain('Only clients that ask for TLS')
  })

  it('explains that Available encrypts only clients that ask, with an example', () => {
    const html = render(availableStatus)
    expect(html).toContain('Only clients that ask for TLS are encrypted')
    expect(html).toContain('sslmode=require')
    expect(html).toContain(VERIFY_FULL_EXAMPLE.replace(/&/g, '&amp;'))
    expect(html).toContain('Linked apps keep their current connection string')
  })

  it('shows the certificate names and the CA fingerprint with a download', () => {
    const html = render(availableStatus)
    expect(html).toContain('postgres-orders, localhost, 127.0.0.1')
    expect(html).toContain('abcdef01')
    expect(html).toContain('Download CA certificate')
    expect(html).toContain('Copy fingerprint')
  })

  it('explains why an unsupported database cannot offer TLS', () => {
    const html = render({
      ...offStatus,
      supported: false,
      unsupported_reason:
        'Server TLS is not supported for PostgreSQL clusters yet.',
    })
    expect(html).toContain('TLS is not available for this database')
    expect(html).toContain('not supported for PostgreSQL clusters yet')
    expect(html).toContain('orders cannot offer TLS.')
    // Off stays selectable; Available and Required are both disabled.
    expect(html.match(/disabled=""/g)?.length).toBe(2)
  })

  it('locks every mode while a change is applied', () => {
    const html = render(offStatus, true)
    expect(html.match(/disabled=""/g)?.length).toBe(3)
    expect(html).toContain('animate-spin')
  })
})

describe('cluster TLS card', () => {
  it('lists every data node with what it serves', () => {
    const html = render(clusterStatus)
    expect(html).toContain('postgres-orders-1')
    expect(html).toContain('postgres-orders-2')
    expect(html).toContain('Managed certificate')
    expect(html).toContain('Managed certificate on 2 of 2 data nodes.')
  })
})
