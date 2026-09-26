// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { mariadbServiceParameters } from './mariadb-image.ts'

const DIGEST = 'a'.repeat(64)

describe('MariaDB restore scenario service parameters', () => {
  test('passes a local immutable image ID to service creation', () => {
    expect(mariadbServiceParameters(`sha256:${DIGEST}`, 'e2etest')).toEqual({
      database: 'e2etest',
      username: 'app',
      docker_image: `sha256:${DIGEST}`,
    })
  })

  test('passes a published repository digest to service creation', () => {
    expect(
      mariadbServiceParameters(
        `ghcr.io/gotempsh/mariadb-walg@sha256:${DIGEST}`,
        'e2etest',
      ),
    ).toEqual({
      database: 'e2etest',
      username: 'app',
      docker_image: `ghcr.io/gotempsh/mariadb-walg@sha256:${DIGEST}`,
    })
  })

  test.each([undefined, '', 'mariadb:11.4', 'ghcr.io/gotempsh/mariadb-walg:11.4'])(
    'rejects missing or mutable image %p before making an API request',
    (image) => {
      expect(() => mariadbServiceParameters(image, 'e2etest')).toThrow(
        'requires --mariadb-image',
      )
    },
  )
})
