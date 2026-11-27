// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  welcomeProjectCount,
  formatBytes,
  connectAgentPrompt,
  deployAppPrompt,
  harnessCommands,
  reachHint,
  nextStep,
  shouldWelcome,
  stepStatus,
} from './onboarding'

describe('onboarding', () => {
  test('the next step skips what is done and what was put off', () => {
    expect(nextStep({ done: [], later: [] })).toBe('host')
    expect(
      nextStep({
        done: ['host'],
        later: [{ step: 'reach', reason: 'no domain yet' }],
      })
    ).toBe('agents')
    expect(
      nextStep({ done: ['host', 'reach', 'agents', 'app'], later: [] })
    ).toBeNull()
  })

  test('the flow opens on its own only on a fresh console', () => {
    expect(shouldWelcome({ done: [], later: [] }, 0)).toBe(true)
    expect(shouldWelcome({ done: [], later: [] }, 2)).toBe(false)
    expect(
      shouldWelcome(
        { completed_at: '2026-11-24T00:00:00Z', done: [], later: [] },
        0
      )
    ).toBe(false)
    expect(shouldWelcome(undefined, 0)).toBe(false)
    expect(shouldWelcome({ done: [], later: [] }, undefined)).toBe(false)
  })

  test('step status carries the reason it was put off', () => {
    const state = {
      done: ['host' as const],
      later: [{ step: 'reach' as const, reason: 'no domain yet' }],
    }
    expect(stepStatus(state, 'host')).toEqual({ kind: 'done' })
    expect(stepStatus(state, 'reach')).toEqual({
      kind: 'later',
      note: 'no domain yet',
    })
    expect(stepStatus(state, 'app')).toEqual({ kind: 'todo' })
  })

  test('helpers', () => {
    expect(harnessCommands('https://temps.example.com')[1]).toBe(
      'bunx @temps-sdk/cli login https://temps.example.com'
    )
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(4 * 1024 ** 3)).toBe('4.0 GB')
    expect(formatBytes(64 * 1024 ** 3)).toBe('64 GB')
    expect(formatBytes(null)).toBe('unknown')
  })
})

test('the skill installs for the user, without questions, so no repository is needed', () => {
  expect(harnessCommands('https://temps.example.com')[0]).toBe(
    'bunx skills add gotempsh/temps --skill temps -g -y'
  )
})

test('the prompt for your agent checks the server first, carries both commands and says when to stop', () => {
  const origin = 'https://temps.example.com'
  const prompt = connectAgentPrompt(origin)
  for (const command of harnessCommands(origin)) {
    expect(prompt).toContain(`   ${command}\n`)
  }
  const reach = prompt.indexOf(
    `curl -sS -o /dev/null -w "%{http_code}\\n" ${origin}`
  )
  expect(reach).toBeGreaterThan(-1)
  expect(reach).toBeLessThan(prompt.indexOf(harnessCommands(origin)[0]))
  expect(prompt).not.toContain('this repository')
  expect(prompt).not.toContain('localhost')
  expect(prompt).toContain('bunx @temps-sdk/cli whoami')
  expect(prompt).toContain(`logged in to ${origin}`)
  expect(prompt).toContain('Show me the link')
  expect(prompt).toContain('Do not use --api-key.')
  expect(prompt.split('\n').pop()).toBe(
    'If a command fails, show me its output and stop. Do not work around it.'
  )
})

test('a console on localhost tells the agent to run on this computer', () => {
  const prompt = connectAgentPrompt('http://localhost:3014')
  expect(prompt.split('\n')[2]).toBe(
    'The server runs on my computer at a localhost address. Run these commands on my computer, in a shell that can reach http://localhost:3014; a cloud workspace or a VM cannot.'
  )
})

test('a console on a private network tells the agent to run where it can reach it', () => {
  for (const origin of [
    'http://192.168.1.10:8080',
    'http://100.101.102.103',
    'https://temps.tailnet-1234.ts.net',
    'http://nas:3000',
  ]) {
    const prompt = connectAgentPrompt(origin)
    expect(prompt.split('\n')[2]).toBe(
      `The server is on a private network. Run these commands on a machine that can reach ${origin}, such as my computer; a cloud workspace or a VM usually cannot.`
    )
  }
})

test('the reach hint names where the agent must run', () => {
  expect(reachHint('http://localhost:3014')).toContain('this computer')
  expect(reachHint('http://[::1]:3000')).toContain('this computer')
  expect(reachHint('http://10.0.0.4')).toContain('private network')
  expect(reachHint('https://temps.example.com')).toBeNull()
})

test('the deploy prompt logs in first when it has to, asks what to deploy, then deploys', () => {
  const origin = 'https://temps.example.test'
  const prompt = deployAppPrompt(origin)
  const [skill, login] = harnessCommands(origin)
  expect(prompt.split('\n')[0]).toBe(
    'Help me put my first app live on my Temps server at https://temps.example.test.'
  )
  // In order: check the login, log in only if needed, interview the user,
  // confirm the plan, then deploy.
  const order = [
    'bunx @temps-sdk/cli whoami',
    `If it shows you logged in to ${origin}, go to step 2.`,
    login,
    'Ask me what I want, one question at a time',
    'deploy an app I already have, or build something new?',
    'Wait for my yes before you change anything on Temps.',
    'create the project, deploy it',
  ].map((text) => prompt.indexOf(text))
  expect(order.every((at) => at > -1)).toBe(true)
  expect([...order].sort((a, b) => a - b)).toEqual(order)
  expect(prompt).toContain(skill)
  expect(prompt).toContain('Do not use --api-key.')
  expect(prompt).toContain('Never ask me to paste a secret into this chat')
  // A public server needs no note on where to run.
  expect(prompt).not.toContain('localhost address')
  expect(deployAppPrompt('http://localhost:3014')).toContain(
    'The server runs on my computer at a localhost address.'
  )
})

describe('welcomeProjectCount', () => {
  test('an empty install opens the welcome flow, unless the user searched or is creating a project', () => {
    expect(welcomeProjectCount(0, { searching: false, creating: false })).toBe(
      0
    )
    expect(welcomeProjectCount(3, { searching: false, creating: false })).toBe(
      3
    )
    expect(
      welcomeProjectCount(0, { searching: true, creating: false })
    ).toBeUndefined()
    // The command palette's Create project: the dialog is not replaced.
    expect(
      welcomeProjectCount(0, { searching: false, creating: true })
    ).toBeUndefined()
    expect(
      welcomeProjectCount(undefined, { searching: false, creating: false })
    ).toBeUndefined()
  })
})
