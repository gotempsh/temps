// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerDeployCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const routes = {
  'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' },
  'GET /projects/7/environments': [{ id: 2, name: 'production', is_preview: false }],
}

describe('deploy:image image sources', () => {
  test('--external-image-id deploys a registered image instead of --image', async () => {
    const run = await runWithApi(
      registerDeployCommands,
      ['deploy:image', '--external-image-id', '15', '-p', 'web', '--no-wait', '--yes'],
      { ...routes, 'POST /projects/7/environments/2/deploy/image': { id: 40, slug: 'd-40' } },
    )
    expect(run.bodies.find((b) => b.request.startsWith('POST'))?.body).toEqual({ external_image_id: 15 })
  })

  test('--claim-local claims an image on the host', async () => {
    const run = await runWithApi(
      registerDeployCommands,
      ['deploy:image', '--image', 'shop:local', '--claim-local', '-p', 'web', '--no-wait', '--yes'],
      { ...routes, 'POST /projects/7/environments/2/deploy/image': { id: 41, slug: 'd-41' } },
    )
    expect(run.bodies.find((b) => b.request.startsWith('POST'))?.body).toEqual({ image_ref: 'shop:local', claim_local: true })
  })

  test('conflicting image sources fail before any request', async () => {
    const both = await runWithApi(registerDeployCommands, ['deploy:image', '--image', 'a:1', '--external-image-id', '15', '-p', 'web', '--yes'], {})
    expect(both.error?.message).toMatch(/name the image twice/)
    const claim = await runWithApi(registerDeployCommands, ['deploy:image', '--external-image-id', '15', '--claim-local', '-p', 'web', '--yes'], {})
    expect(claim.error?.message).toMatch(/--claim-local takes an image on the host/)
    expect([...both.requests, ...claim.requests]).toEqual([])
  })
})
