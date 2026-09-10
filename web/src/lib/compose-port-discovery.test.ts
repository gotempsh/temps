// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  findComposePortMapping,
  mergeComposeServicePorts,
  parseComposeOverridePorts,
} from './compose-port-discovery'

describe('Compose port discovery', () => {
  test('reads container targets from short and long YAML mappings', () => {
    expect(
      parseComposeOverridePorts(`
services:
  web:
    ports:
      - "15455:80"
      - target: 443
        published: 15443
        protocol: tcp
`)
    ).toEqual([
      {
        name: 'web',
        ports: [
          { target: 80, published: 15455, protocol: 'tcp' },
          { target: 443, published: 15443, protocol: 'tcp' },
        ],
      },
    ])
  })

  test('combines repository and override ports by service and target', () => {
    const merged = mergeComposeServicePorts(
      [{ name: 'web', ports: [{ target: 3000, protocol: 'tcp' }] }],
      parseComposeOverridePorts(
        'services:\n  web:\n    ports:\n      - "15455:80"'
      )
    )
    expect(merged[0].ports.map((port) => port.target)).toEqual([80, 3000])
  })

  test('preserves service image metadata while combining ports', () => {
    const merged = mergeComposeServicePorts(
      [{ name: 'web', image: 'nginx:alpine', ports: [] }],
      [{ name: 'web', ports: [{ target: 80, protocol: 'tcp' }] }]
    )

    expect(merged).toEqual([
      {
        name: 'web',
        image: 'nginx:alpine',
        ports: [{ target: 80, protocol: 'tcp' }],
      },
    ])
  })

  test('resolves a Compose mapping from either its target or published port', () => {
    const services = parseComposeOverridePorts(
      'services:\n  nc:\n    ports:\n      - "15455:80"'
    )
    expect(findComposePortMapping(services, 'nc', 15455)?.target).toBe(80)
    expect(findComposePortMapping(services, 'nc', 80)?.published).toBe(15455)
    expect(findComposePortMapping(services, 'nc', 443)).toBeUndefined()
  })
})
