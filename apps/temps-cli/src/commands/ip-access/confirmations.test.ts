// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test'
import {
  confirmationFlag,
  confirmationMessage,
  ipRuleConfirmation,
  saveWithConfirmations,
  type IpRuleConfirmationFields,
  type SaveResult,
} from './confirmations.js'
import { registerIpAccessCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const BROAD = {
  type: 'about:blank',
  status: 409,
  title: 'Block Range Needs Confirmation',
  detail:
    'Block rule fc00::/7 covers a /7 range, wider than the /16 minimum for IPv6; resubmit with ' +
    'confirm_broad_range=true if you mean to block this much of the internet',
}

const LOCKOUT = {
  type: 'about:blank',
  status: 409,
  title: 'Rule Would Lock You Out',
  detail:
    'Block rule 198.51.100.0/24 matches your own address 198.51.100.7, so you and every other visitor ' +
    'from it would be refused by the proxy, the console included; resubmit with acknowledge_lockout=true ' +
    'if that is intended',
}

const DUPLICATE = { type: 'about:blank', status: 409, title: 'Duplicate IP Address', detail: 'Duplicate IP address: x' }

const RULE = {
  id: 3,
  ip_address: 'fc00::/7',
  action: 'block',
  reason: null,
  created_by: 1,
  created_at: '2026-11-25T10:00:00Z',
  updated_at: '2026-11-25T10:00:00Z',
}

function conflict(problem: object): Response {
  return Response.json(problem, { status: 409, headers: { 'content-type': 'application/problem+json' } })
}

/** A server that refuses until the request carries every field in `needs`. */
function refusingUntil(needs: Array<keyof IpRuleConfirmationFields>) {
  return (_url: URL, body: unknown) => {
    const sent = body as Partial<IpRuleConfirmationFields>
    if (needs.includes('confirm_broad_range') && sent.confirm_broad_range !== true) return conflict(BROAD)
    if (needs.includes('acknowledge_lockout') && sent.acknowledge_lockout !== true) return conflict(LOCKOUT)
    return RULE
  }
}

describe('ipRuleConfirmation', () => {
  test('recognises the two confirmations by title on a 409', () => {
    expect(ipRuleConfirmation(BROAD, 409)).toBe('broad_range')
    expect(ipRuleConfirmation(LOCKOUT, 409)).toBe('lockout')
  })

  test('ignores other conflicts and other statuses', () => {
    expect(ipRuleConfirmation(DUPLICATE, 409)).toBeNull()
    expect(ipRuleConfirmation(BROAD, 400)).toBeNull()
    expect(ipRuleConfirmation(BROAD, undefined)).toBeNull()
    expect(ipRuleConfirmation('Block Range Needs Confirmation', 409)).toBeNull()
    expect(ipRuleConfirmation(null, 409)).toBeNull()
  })
})

describe('confirmationMessage', () => {
  test('names the CLI flag instead of the API field', () => {
    const broad = confirmationMessage('broad_range', BROAD)
    expect(broad).toContain('fc00::/7 covers a /7 range')
    expect(broad).toContain('--confirm-broad-range')
    expect(broad).not.toContain('confirm_broad_range=true')

    const lockout = confirmationMessage('lockout', LOCKOUT)
    expect(lockout).toContain('matches your own address 198.51.100.7')
    expect(lockout).toContain('--acknowledge-lockout')
    expect(lockout).not.toContain('acknowledge_lockout=true')
  })

  test('maps each confirmation to its flag', () => {
    expect(confirmationFlag('broad_range')).toBe('--confirm-broad-range')
    expect(confirmationFlag('lockout')).toBe('--acknowledge-lockout')
  })
})

describe('saveWithConfirmations on a terminal', () => {
  let stdinTty: boolean | undefined
  beforeEach(() => {
    stdinTty = process.stdin.isTTY
    process.stdin.isTTY = true
  })
  afterEach(() => {
    process.stdin.isTTY = stdinTty as boolean
  })

  async function save(
    needs: Array<keyof IpRuleConfirmationFields>,
    answers: boolean[],
    interactive = true
  ): Promise<{ result: unknown; sent: IpRuleConfirmationFields[]; asked: string[] }> {
    const sent: IpRuleConfirmationFields[] = []
    const asked: string[] = []
    const errSpy = spyOn(console, 'error').mockImplementation(() => {})
    try {
      const respond = refusingUntil(needs)
      const result = await saveWithConfirmations(
        'Saving...',
        async (fields): Promise<SaveResult<typeof RULE>> => {
          sent.push(fields)
          const response = respond(new URL('http://127.0.0.1/'), fields)
          if (response instanceof Response) {
            return { error: await response.json(), response }
          }
          return { data: response, response: new Response(null, { status: 201 }) }
        },
        { interactive },
        async (question) => {
          asked.push(question)
          return answers.shift() ?? false
        }
      )
      return { result, sent, asked }
    } finally {
      errSpy.mockRestore()
    }
  }

  test('asks for each confirmation in turn and resends with it', async () => {
    const { result, sent, asked } = await save(['confirm_broad_range', 'acknowledge_lockout'], [true, true])
    expect(result).toEqual({ saved: RULE })
    expect(asked).toEqual([
      'Block this much of the internet?',
      'This rule would lock you out, the console included. Save it anyway?',
    ])
    expect(sent).toEqual([
      { confirm_broad_range: false, acknowledge_lockout: false },
      { confirm_broad_range: true, acknowledge_lockout: false },
      { confirm_broad_range: true, acknowledge_lockout: true },
    ])
  })

  test('declining saves nothing more', async () => {
    const { result, sent } = await save(['acknowledge_lockout'], [false])
    expect(result).toBeUndefined()
    expect(sent).toHaveLength(1)
  })

  test('does not ask when prompts are off (--yes); names the flag instead', async () => {
    await expect(save(['confirm_broad_range'], [true], false)).rejects.toThrow('--confirm-broad-range')
  })
})

describe('ip-access create and update', () => {
  const register = registerIpAccessCommands

  test('-y does not confirm a broad range: the error names the flag', async () => {
    const run = await runWithApi(
      register,
      ['ip-access', 'create', '--ip', 'fc00::/7', '--action', 'deny', '-y'],
      { 'POST /ip-access-control': refusingUntil(['confirm_broad_range']) }
    )
    expect(run.error?.message).toContain('Re-run with --confirm-broad-range')
    expect(run.error?.message).not.toContain('confirm_broad_range=true')
    expect(run.bodies).toEqual([
      {
        request: 'POST /ip-access-control',
        body: {
          ip_address: 'fc00::/7',
          action: 'block',
          reason: null,
          confirm_broad_range: false,
          acknowledge_lockout: false,
        },
      },
    ])
  })

  test('--confirm-broad-range sends the confirmation', async () => {
    const run = await runWithApi(
      register,
      ['ip-access', 'create', '--ip', 'fc00::/7', '--action', 'deny', '--confirm-broad-range', '-y'],
      { 'POST /ip-access-control': refusingUntil(['confirm_broad_range']) }
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies).toHaveLength(1)
    expect(run.bodies[0]?.body).toMatchObject({ confirm_broad_range: true, acknowledge_lockout: false })
    expect(run.stdout).toContain('IP access control rule created (ID: 3)')
  })

  test('update --acknowledge-lockout sends the acknowledgement', async () => {
    const run = await runWithApi(
      register,
      ['ip-access', 'update', '--id', '3', '--ip', '198.51.100.0/24', '--acknowledge-lockout'],
      { 'PATCH /ip-access-control/3': refusingUntil(['acknowledge_lockout']) }
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([
      {
        request: 'PATCH /ip-access-control/3',
        body: { ip_address: '198.51.100.0/24', confirm_broad_range: false, acknowledge_lockout: true },
      },
    ])
    expect(run.stdout).toContain('Rule #3 updated')
  })

  test('update without a terminal names the flag instead of asking', async () => {
    const run = await runWithApi(
      register,
      ['ip-access', 'update', '--id', '3', '--ip', '198.51.100.0/24'],
      { 'PATCH /ip-access-control/3': refusingUntil(['acknowledge_lockout']) },
      { stdin: '' }
    )
    expect(run.error?.message).toContain('Re-run with --acknowledge-lockout')
    expect(run.bodies).toHaveLength(1)
  })

  test('another conflict keeps the server message', async () => {
    const run = await runWithApi(
      register,
      ['ip-access', 'create', '--ip', '203.0.113.9', '--action', 'deny', '-y'],
      { 'POST /ip-access-control': conflict(DUPLICATE) }
    )
    expect(run.error?.message).toBe('Duplicate IP address: x')
  })
})
