// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildCreateRuleRequest,
  describePorts,
  describeSubject,
  effectivePolicyEnforcementText,
  profileUpdate,
  nodeNetworkPolicyMode,
  ruleDeleteBlockedReason,
  rulesQuery,
  type EffectivePolicy,
  type NetworkPolicyRule,
  type RuleFormValues,
} from './network-policy'

function form(overrides: Partial<RuleFormValues>): RuleFormValues {
  return {
    subject_kind: 'sandbox',
    subject_id: '42',
    direction: 'egress',
    peer_kind: 'host',
    peer_ref: 'registry.example.test',
    port_start: '',
    port_end: '',
    protocol: 'tcp',
    action: 'allow',
    description: '',
    ...overrides,
  }
}

function rule(overrides: Partial<NetworkPolicyRule>): NetworkPolicyRule {
  return {
    id: 1,
    subject_kind: 'sandbox',
    subject_id: 42,
    direction: 'egress',
    peer_kind: 'service',
    peer_ref: '12',
    port_start: null,
    port_end: null,
    protocol: 'tcp',
    action: 'allow',
    origin: 'operator',
    link_table: null,
    link_id: null,
    description: null,
    created_by: 1,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  }
}

describe('rulesQuery', () => {
  test('sends the subject filter only as a pair', () => {
    expect(rulesQuery({ subject_kind: 'sandbox', page: 1 })).toEqual({
      page: 1,
    })
    expect(
      rulesQuery({ subject_kind: 'service', subject_id: 3, origin: 'link' })
    ).toEqual({ subject_kind: 'service', subject_id: 3, origin: 'link' })
  })
})

describe('buildCreateRuleRequest', () => {
  test('omits empty optional fields', () => {
    expect(buildCreateRuleRequest(form({}))).toEqual({
      ok: true,
      value: {
        subject_kind: 'sandbox',
        subject_id: 42,
        direction: 'egress',
        peer_kind: 'host',
        peer_ref: 'registry.example.test',
        protocol: 'tcp',
        action: 'allow',
      },
    })
  })

  test('sends a port range and description', () => {
    const result = buildCreateRuleRequest(
      form({
        peer_kind: 'cidr',
        peer_ref: '10.0.0.0/8',
        port_start: '8000',
        port_end: '8100',
        description: ' internal APIs ',
      })
    )
    expect(result.ok && result.value).toMatchObject({
      port_start: 8000,
      port_end: 8100,
      description: 'internal APIs',
    })
  })

  test('refuses a host peer on ingress', () => {
    const result = buildCreateRuleRequest(form({ direction: 'ingress' }))
    expect(result.ok).toBe(false)
  })

  test('refuses a last port without a first, and bad ranges', () => {
    expect(buildCreateRuleRequest(form({ port_end: '80' })).ok).toBe(false)
    expect(buildCreateRuleRequest(form({ port_start: '70000' })).ok).toBe(false)
    expect(
      buildCreateRuleRequest(form({ port_start: '90', port_end: '80' })).ok
    ).toBe(false)
  })

  test('names the environment for a deployment subject', () => {
    const result = buildCreateRuleRequest(
      form({ subject_kind: 'deployment', subject_id: '0' })
    )
    expect(!result.ok && result.error).toContain('environment ID')
  })
})

describe('display helpers', () => {
  test('describePorts', () => {
    expect(describePorts(rule({}))).toBe('every port/tcp')
    expect(describePorts(rule({ peer_kind: 'host', protocol: 'any' }))).toBe(
      '80, 443'
    )
    expect(describePorts(rule({ port_start: 443, port_end: 443 }))).toBe(
      '443/tcp'
    )
    expect(
      describePorts(rule({ port_start: 1, port_end: 9, protocol: 'udp' }))
    ).toBe('1-9/udp')
  })

  test('describeSubject calls a deployment an environment', () => {
    expect(describeSubject('deployment', 7)).toBe('environment 7')
    expect(describeSubject('sandbox', 7)).toBe('sandbox 7')
  })

  test('link rules are not deleted directly', () => {
    expect(ruleDeleteBlockedReason(rule({ origin: 'link' }))).toContain(
      'remove the link'
    )
    expect(ruleDeleteBlockedReason(rule({}))).toBeNull()
  })

  test('nodeNetworkPolicyMode finds the node', () => {
    const list = {
      nodes: [
        {
          node_id: 0,
          name: 'control-plane',
          role: 'control',
          status: 'online',
          mode: 'shadow',
        },
      ],
    }
    expect(nodeNetworkPolicyMode(list, 0)?.mode).toBe('shadow')
    expect(nodeNetworkPolicyMode(list, 5)).toBeUndefined()
  })

  test('enforcement text names the nodes', () => {
    const policy: EffectivePolicy = {
      subject_kind: 'sandbox',
      subject_id: 1,
      profile: {
        egress: 'restricted',
        ingress: 'open',
        egress_source: 'default',
        ingress_source: 'default',
      },
      rules: [],
      inbound_links: [],
      enforcing_nodes: [0, 3],
      enforced: true,
    }
    expect(effectivePolicyEnforcementText(policy)).toBe(
      'Enforced on the control plane, node 3.'
    )
    expect(
      effectivePolicyEnforcementText({
        ...policy,
        enforced: false,
        enforcing_nodes: [],
      })
    ).toContain('not enforced')
  })
})

describe('profileUpdate', () => {
  const current = {
    egress: 'restricted',
    ingress: 'none',
    egress_source: 'default',
    ingress_source: 'override',
  }
  test('keeps the other direction override', () => {
    expect(profileUpdate(current, 'egress', 'open')).toEqual({
      egress: 'open',
      ingress: 'none',
    })
  })
  test('resetting a direction omits it and drops a defaulted one', () => {
    expect(profileUpdate(current, 'ingress', undefined)).toEqual({})
  })
})
