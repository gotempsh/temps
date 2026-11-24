// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { DEPLOYMENTS_TAB, sidebarSelection } from './sidebar-selection'

const tabs = ['made', 'preview', DEPLOYMENTS_TAB, 'project:7']
const deploymentKeys = ['3:44', '3:43', '5:latest']
const options = { tabs, deploymentKeys, fallback: 'made' }

describe('sidebarSelection', () => {
  test('a deployment opens the Deployments tab on that deployment', () => {
    expect(sidebarSelection('3:43', options)).toEqual({
      tab: DEPLOYMENTS_TAB,
      deployment: '3:43',
    })
  })

  test('the Deployments tab without a pick shows the newest', () => {
    expect(sidebarSelection(DEPLOYMENTS_TAB, options)).toEqual({
      tab: DEPLOYMENTS_TAB,
      deployment: '3:44',
    })
  })

  test('any other tab opens as it is', () => {
    expect(sidebarSelection('project:7', options).tab).toBe('project:7')
    expect(sidebarSelection('preview', options).tab).toBe('preview')
  })

  test('a deployment this chat no longer watches falls back', () => {
    expect(sidebarSelection('3:41', options).tab).toBe('made')
  })

  test('the Deployments tab falls back when there is nothing in it', () => {
    expect(
      sidebarSelection(DEPLOYMENTS_TAB, {
        tabs: ['made'],
        deploymentKeys: [],
        fallback: 'made',
      })
    ).toEqual({ tab: 'made', deployment: null })
  })
})
