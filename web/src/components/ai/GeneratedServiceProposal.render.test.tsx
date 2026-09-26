// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import {
  GeneratedServiceProposal,
  serviceProposalViewModel,
} from './GeneratedServiceProposal'

const PROVIDERS = [
  ['mariadb', 'MariaDB', 'MariaDB logo'],
  ['mongodb', 'MongoDB', 'MongoDB logo'],
  ['postgres', 'PostgreSQL', 'PostgreSQL logo'],
  ['redis', 'Redis', 'Redis logo'],
  ['s3', 'S3 / RustFS', 'S3 / RustFS logo'],
] as const

describe('GeneratedServiceProposal', () => {
  test.each(PROVIDERS)(
    'renders the native %s identity',
    (serviceType, displayName, logoLabel) => {
      const proposal = serviceProposalViewModel(
        JSON.stringify({
          name: `${serviceType}-service`,
          service_type: serviceType,
          version: 'latest',
          parameters: {},
        })
      )

      expect(proposal).not.toBeNull()
      const markup = renderToStaticMarkup(
        <MemoryRouter>
          <GeneratedServiceProposal
            proposal={proposal!}
            statusLabel="Awaiting your confirmation"
          />
        </MemoryRouter>
      )

      expect(markup).toContain(`Create ${displayName}`)
      expect(markup).toContain(`aria-label="${logoLabel}"`)
      expect(markup).toContain('Version latest')
    }
  )

  test('links an executed provider card to its trusted numeric service id', () => {
    const proposal = serviceProposalViewModel(
      JSON.stringify({
        name: 'cache',
        service_type: 'redis',
        version: '8',
        parameters: {},
      })
    )

    const markup = renderToStaticMarkup(
      <MemoryRouter>
        <GeneratedServiceProposal
          proposal={proposal!}
          statusLabel="Executed"
          serviceId={42}
        />
      </MemoryRouter>
    )

    expect(markup).toContain('Redis created')
    expect(markup).toContain('href="/storage/42"')
    expect(markup).toContain('View service')
  })
})
