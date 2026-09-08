// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  createdServiceId,
  serviceProposalViewModel,
} from './GeneratedServiceProposal'

describe('createdServiceId', () => {
  test('returns the numeric id from a successful service creation result', () => {
    expect(
      createdServiceId(
        JSON.stringify({ id: 42, name: 'orders-db', service_type: 'postgres' })
      )
    ).toBe(42)
  })

  test('does not turn malformed or model-authored paths into service links', () => {
    expect(createdServiceId('{not-json')).toBeNull()
    expect(createdServiceId(JSON.stringify({ id: '../settings' }))).toBeNull()
    expect(createdServiceId(JSON.stringify({ id: -1 }))).toBeNull()
  })
})

describe('serviceProposalViewModel', () => {
  test('builds a native PostgreSQL proposal from validated action params', () => {
    expect(
      serviceProposalViewModel(
        JSON.stringify({
          name: 'orders-db',
          service_type: 'postgres',
          version: '18',
          parameters: {
            database: 'orders',
            username: 'postgres',
          },
        })
      )
    ).toEqual({
      serviceType: 'postgres',
      serviceName: 'orders-db',
      displayName: 'PostgreSQL',
      accentClassName: 'text-[#4169E1] dark:text-[#7c9cff]',
      version: '18',
      topology: 'Standalone',
      placement: 'Automatic placement',
      fields: [
        { key: 'database', label: 'Database', value: 'orders' },
        { key: 'username', label: 'Username', value: 'postgres' },
      ],
      secretsProtected: true,
      secretDescription:
        'Database credentials are generated or resolved by Temps and are never exposed to the AI.',
    })
  })

  test('never includes secret-shaped values in the presentation model', () => {
    const proposal = serviceProposalViewModel(
      JSON.stringify({
        name: 'private-db',
        service_type: 'postgres',
        version: '18',
        parameters: {
          database: 'private',
          username: 'postgres',
          password: 'must-not-render',
          api_token: 'must-not-render-either',
        },
      })
    )

    expect(JSON.stringify(proposal)).not.toContain('must-not-render')
    expect(proposal?.secretsProtected).toBe(true)
  })

  test('falls back for malformed or unsupported proposal payloads', () => {
    expect(serviceProposalViewModel('{not-json')).toBeNull()
    expect(
      serviceProposalViewModel(
        JSON.stringify({ service_type: 'unknown', name: 'service' })
      )
    ).toBeNull()
  })

  test.each([
    ['mariadb', 'MariaDB', 'database', 'app'],
    ['mongodb', 'MongoDB', 'replica_set', 'rs0'],
    ['redis', 'Redis', 'port', '6379'],
    ['s3', 'S3 / RustFS', 'region', 'eu-west-1'],
    ['rustfs', 'RustFS', 'console_port', '9001'],
  ])(
    'renders native %s metadata and prioritizes its provider fields',
    (serviceType, displayName, fieldName, fieldValue) => {
      const proposal = serviceProposalViewModel(
        JSON.stringify({
          name: `${serviceType}-service`,
          service_type: serviceType,
          version: 'latest',
          parameters: {
            [fieldName]: fieldValue,
            password: 'never-render-this',
          },
        })
      )

      expect(proposal?.displayName).toBe(displayName)
      expect(proposal?.fields).toContainEqual({
        key: fieldName,
        label: expect.any(String),
        value: fieldValue,
      })
      expect(JSON.stringify(proposal)).not.toContain('never-render-this')
    }
  )
})
