// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { mintRequest, networkRequest, pageQuery, ROTATE_CONFIRMATION } from './admin.js'
import { registerClusterCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('cluster admin helpers', () => {
  test('pageQuery, mintRequest and networkRequest validate their flags', () => {
    expect(pageQuery({ page: '2', pageSize: '50' })).toEqual({ page: 2, page_size: 50 })
    expect(() => pageQuery({ pageSize: '500' })).toThrow(/--page-size/)
    expect(mintRequest({ ttl: '600', maxUses: '1', node: 'worker-b' })).toEqual({ ttl_secs: 600, max_uses: 1, bound_node_name: 'worker-b' })
    expect(() => mintRequest({ ttl: '5' })).toThrow(/--ttl/)
    expect(networkRequest({ computePool: '10.200.0.0/16', subnetPrefix: '24' })).toEqual({ compute_pool_cidr: '10.200.0.0/16', subnet_prefix_len: 24 })
    expect(() => networkRequest({})).toThrow(/--compute-pool/)
  })
})

describe('temps cluster (admin)', () => {
  const routes = {
    'GET /cluster/dns/resolvers': { resolvers: [] },
    'GET /cluster/dns/records': { records: [] },
    'GET /cluster/dns/queries': { queries: [] },
    'GET /cluster/dns/traffic': { points: [] },
    'POST /cluster/dns/resolve': { answers: [], authoritative: true },
    'GET /settings/cluster-network': { can_change: true },
    'PUT /settings/cluster-network': { changed: true },
    'POST /settings/cluster-network/network-id': { network_id: 7 },
    'GET /settings/join-token/status': { has_token: false },
    'POST /settings/join-token/generate': { token: 'jt_once', message: 'ok' },
    'DELETE /settings/join-token': { success: true, message: 'revoked' },
    'GET /settings/enrollment-tokens': { tokens: [] },
    'POST /settings/enrollment-tokens': { id: 4, token: 'et_once', expires_at: '2026-01-01T00:00:00Z', max_uses: 1, message: 'ok' },
    'DELETE /settings/enrollment-tokens/4': { success: true, message: 'revoked' },
    'POST /settings/cluster-ca/rotate': { message: 'rotated', new_fingerprint: 'b', previous_fingerprint: 'a', revoked_enrollment_tokens: 2 },
  }

  test.each([
    [['dns', 'resolvers'], ['GET /cluster/dns/resolvers']],
    [['dns', 'records', '--search', 'web', '--node', '1', '--owner-kind', 'service', '--page', '2'], ['GET /cluster/dns/records?page=2&search=web&node_id=1&owner_kind=service']],
    [['dns', 'queries', '--outcome', 'nxdomain', '--zone', 'temps.internal', '--name', 'db', '--project', '5', '--page-size', '10'], ['GET /cluster/dns/queries?page_size=10&outcome=nxdomain&zone=temps.internal&name=db&project_id=5']],
    [['dns', 'traffic', '--node', '0', '--range', '24h'], ['GET /cluster/dns/traffic?node_id=0&range=24h']],
    [['dns', 'resolve', 'db.temps.internal', '--node', '1', '--type', 'A', '--sandbox', 'sbx_1'], ['POST /cluster/dns/resolve']],
    [['network', 'show'], ['GET /settings/cluster-network']],
    [['network', 'set', '--compute-pool', '10.200.0.0/16', '--yes'], ['PUT /settings/cluster-network']],
    [['network', 'new-network-id', '--yes'], ['POST /settings/cluster-network/network-id']],
    [['join-token', 'status'], ['GET /settings/join-token/status']],
    [['join-token', 'generate'], ['GET /settings/join-token/status', 'POST /settings/join-token/generate']],
    [['join-token', 'revoke', '--yes'], ['DELETE /settings/join-token']],
    [['enrollment-tokens', 'list'], ['GET /settings/enrollment-tokens']],
    [['enrollment-tokens', 'create', '--ttl', '600'], ['POST /settings/enrollment-tokens']],
    [['enrollment-tokens', 'revoke', '4'], ['DELETE /settings/enrollment-tokens/4']],
    [['rotate-ca', '--expected-fingerprint', 'a', '--yes'], ['POST /settings/cluster-ca/rotate']],
  ])('cluster %p', async (argv, requests) => {
    const run = await runWithApi(registerClusterCommands, ['cluster', ...argv, '--json'], routes)
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(requests)
  })

  test('a minted token is printed once, and rotate-ca sends the required confirmation', async () => {
    const minted = await runWithApi(registerClusterCommands, ['cluster', 'enrollment-tokens', 'create'], routes)
    expect(minted.stdout).toContain('et_once')
    expect(minted.stderr + minted.stdout).toMatch(/shown only once/)
    const rotate = await runWithApi(registerClusterCommands, ['cluster', 'rotate-ca', '--expected-fingerprint', 'a', '--yes'], routes)
    expect(rotate.bodies[0]?.body).toEqual({ expected_fingerprint: 'a', confirmation: ROTATE_CONFIRMATION })
  })

  test('generate asks before replacing an existing join token, and rotate-ca refuses without a terminal', async () => {
    const replace = await runWithApi(
      registerClusterCommands,
      ['cluster', 'join-token', 'generate'],
      { ...routes, 'GET /settings/join-token/status': { has_token: true } },
      { stdin: '' },
    )
    expect(replace.requests).toEqual(['GET /settings/join-token/status'])
    const rotate = await runWithApi(registerClusterCommands, ['cluster', 'rotate-ca', '--expected-fingerprint', 'a'], routes, { stdin: '' })
    expect(rotate.requests).toEqual([])
  })
})
