// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// A `useMutation` without its own `onError` is already announced by the
// app-wide handler in App.tsx (authored `meta.errorTitle` + the server's
// reason). A second `toast.error` for the same failure -- in the `catch` around
// `x.mutateAsync(...)`, or as the `error:` message of a `toast.promise` wrapped
// around it -- shows the user two toasts. Wording that is worth keeping belongs
// in the mutation's `meta.errorTitle`. This is a source guard; the behavioural
// check lives in `*.failure-toast.test.tsx` next to the components.
//
// Entries are `file -> mutation variables`, relative to `web/src`.
const GUARDED: Record<string, string[]> = {
  'components/ai-first/ApplicationDataServicesPanel.tsx': [
    'linkMutation',
    'unlinkMutation',
  ],
  'components/observe/CloudTelemetryActivationSection.tsx': [
    'estimateMutation',
    'createMutation',
    'cancelMutation',
  ],
  'components/project-groups/DisconnectRepositoryDialog.tsx': ['clear'],
  'components/project-groups/FollowedRepositoryNotice.tsx': ['stop'],
  'components/project-groups/GroupRepositoryPanel.tsx': ['reapply', 'follow'],
  'components/project-groups/GroupRoutesSettings.tsx': ['remove'],
  'components/project-groups/GroupSettingsPanel.tsx': ['remove', 'destroy'],
  'components/project-groups/GroupVariablesPanel.tsx': ['remove'],
  'components/project-groups/ServiceGroupControl.tsx': ['add'],
  'components/project/ProjectDeployments.tsx': [
    'deployImageMut',
    'deployFromStaticMut',
  ],
  'components/project/settings/GeneralSettings.tsx': [
    'updateProjectSettings',
    'deleteProjectMutationM',
  ],
  'components/project/settings/GitSettings.tsx': [
    'reinstallWebhook',
    'updateAutomaticDeploy',
    'updateGit',
  ],
  'pages/CreateUser.tsx': ['addTeamMember'],
  'pages/GitSources.tsx': ['deleteProviderMut'],
  'pages/settings/CloudSettingsPage.tsx': [
    'enroll',
    'disconnect',
    'updateFeatures',
    'reconcileBackupSource',
    'ensureBackupSchedule',
    'repointArchive',
  ],
  'pages/settings/NodesPage.tsx': ['generateToken', 'revokeToken'],
  'pages/agent-sandbox/AgentSandboxProviderDetail.tsx': [
    'refreshModelsMutation',
  ],
  // `toast.promise` wrappers.
  'components/domains/DomainsManagement.tsx': ['renewDomain'],
  'components/project/ProjectStorage.tsx': [
    'linkServiceMutation',
    'unlinkServiceMutation',
  ],
  'components/project/settings/DeployDefaultsCard.tsx': [
    'updateDeploymentConfig',
  ],
  'components/project/settings/EnvironmentPortOverrideCard.tsx': [
    'updateSettings',
  ],
  'components/project/settings/ImageRetentionCard.tsx': [
    'updateProjectSettings',
  ],
  'components/project/settings/MonitoringCard.tsx': ['updateDeploymentConfig'],
  'components/project/settings/PreviewEnvironmentsCard.tsx': [
    'updateProjectSettings',
  ],
  'components/project/settings/ProjectSecuritySettings.tsx': [
    'updateProjectSettings',
    'updateDeploymentConfig',
  ],
  'components/project/settings/ServiceTemplateRuntimeCard.tsx': [
    'updateRuntime',
    'upgrade',
  ],
  'pages/DomainDetail.tsx': ['cancelOrder'],
  'pages/ServiceDetail.tsx': ['startService'],
}

// `useUpdateSettings` toasts in its own `onError`; its callers must not.
const UPDATE_SETTINGS_CALLERS: Record<string, string[]> = {
  'components/settings/AiChatLimitsCard.tsx': ['updateSettings'],
  'pages/Settings.tsx': ['updateSettings'],
  'pages/agent-sandbox/AgentSandboxSandboxPage.tsx': ['updateSettings'],
  'pages/settings/BuildLimitsPage.tsx': ['updateSettings'],
  'pages/settings/DiskMonitoringPage.tsx': ['updateSettings'],
  'pages/settings/DockerRegistryPage.tsx': ['updateSettings'],
  'pages/settings/MonitoringSettingsPage.tsx': [
    'updateSettings',
    'removeLicenseKey',
  ],
  'pages/settings/RateLimitingPage.tsx': ['updateSettings'],
  'pages/settings/RequestTimeoutsPage.tsx': ['updateSettings'],
}

const SRC = join(import.meta.dir, '..')

/**
 * Index of the bracket matching the one at `open`, ignoring string literals
 * and comments.
 */
function matching(source: string, open: number): number {
  const pairs: Record<string, string> = { '(': ')', '{': '}', '[': ']' }
  const stack: string[] = []
  let quote: string | null = null
  for (let i = open; i < source.length; i++) {
    const c = source[i]!
    if (!quote && c === '/' && source[i + 1] === '/') {
      i = source.indexOf('\n', i)
      if (i < 0) return -1
    } else if (!quote && c === '/' && source[i + 1] === '*') {
      i = source.indexOf('*/', i + 2) + 1
      if (i < 1) return -1
    } else if (quote) {
      if (c === '\\') i++
      else if (c === quote) quote = null
    } else if (c === "'" || c === '"' || c === '`') {
      quote = c
    } else if (pairs[c]) {
      stack.push(pairs[c]!)
    } else if (stack.length > 0 && c === stack[stack.length - 1]) {
      stack.pop()
      if (stack.length === 0) return i
    }
  }
  return -1
}

/** Bodies of the `catch` clauses of every `try` block that awaits `callee`. */
export function catchBodiesAround(source: string, callee: string): string[] {
  const bodies: string[] = []
  const tryMarker = /\btry\s*\{/g
  for (let m = tryMarker.exec(source); m; m = tryMarker.exec(source)) {
    const open = source.indexOf('{', m.index)
    const close = matching(source, open)
    if (
      close < 0 ||
      !source.slice(open, close).includes(`${callee}.mutateAsync`)
    )
      continue
    const catchMarker = /^\s*catch\s*(\([^)]*\))?\s*\{/.exec(
      source.slice(close + 1)
    )
    if (!catchMarker) continue
    const catchOpen = close + catchMarker[0].length
    bodies.push(source.slice(catchOpen + 1, matching(source, catchOpen)))
  }
  return bodies
}

/**
 * `toast.promise(...)` calls that wrap `callee.mutateAsync(...)`, directly or
 * through a variable holding its promise.
 */
export function promiseCallsAround(source: string, callee: string): string[] {
  const calls: string[] = []
  const marker = /toast\.promise\s*\(/g
  for (let m = marker.exec(source); m; m = marker.exec(source)) {
    const open = source.indexOf('(', m.index)
    const call = source.slice(open, matching(source, open) + 1)
    const first = /^\(\s*(\w+)\s*,/.exec(call)?.[1]
    const viaVariable =
      first !== undefined &&
      new RegExp(
        `(const|let)\\s+${first}\\s*=\\s*${callee}\\.mutateAsync`
      ).test(source)
    if (viaVariable || call.includes(`${callee}.mutateAsync`)) calls.push(call)
  }
  return calls
}

/** Whether `toast.promise` options (second argument) define an `error` key. */
function definesErrorMessage(call: string): boolean {
  return /\n\s*error\s*:/.test(call) || /,\s*error\s*:/.test(call)
}

describe('source guard helpers', () => {
  test('catchBodiesAround finds the catch of the try that awaits the mutation', () => {
    const source = `
      try { await a.mutateAsync(1); if (x) { y() } } catch (e) { toast.error('a') }
      try { await b.mutateAsync(1) } catch { handled() }
    `
    expect(catchBodiesAround(source, 'a')).toEqual([" toast.error('a') "])
    expect(catchBodiesAround(source, 'b')).toEqual([' handled() '])
  })

  test('promiseCallsAround only returns toast.promise calls around the mutation', () => {
    const source = `
      toast.promise(a.mutateAsync(1), { loading: 'x', error: 'bad' })
      toast.promise(other(), { loading: 'y' })
      const pending = c.mutateAsync(2)
      toast.promise(pending, { loading: 'z', error: 'bad' })
    `
    const calls = promiseCallsAround(source, 'a')
    expect(calls).toHaveLength(1)
    expect(definesErrorMessage(calls[0]!)).toBe(true)
    expect(promiseCallsAround(source, 'c')).toHaveLength(1)
  })
})

describe('mutation failures are announced once', () => {
  for (const [file, mutations] of Object.entries({
    ...GUARDED,
    ...UPDATE_SETTINGS_CALLERS,
  })) {
    test(`${file} leaves the failure toast to the owner of the mutation`, () => {
      const source = readFileSync(join(SRC, file), 'utf8')
      for (const mutation of mutations) {
        expect(source).toContain(`${mutation}.mutateAsync`)
        for (const body of catchBodiesAround(source, mutation)) {
          expect(body).not.toMatch(/toast\.(error|warning)/)
        }
        for (const call of promiseCallsAround(source, mutation)) {
          expect(definesErrorMessage(call)).toBe(false)
        }
      }
    })
  }
})
