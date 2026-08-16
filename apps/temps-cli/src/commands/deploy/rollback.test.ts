import { test, expect, describe } from 'bun:test'
import { filterRollbackCandidates, formatRollbackChoice } from './rollback.js'
import type { DeploymentResponse } from '../../api/types.gen.js'

function makeDeployment(overrides: Partial<DeploymentResponse> = {}): DeploymentResponse {
  return {
    id: 1,
    project_id: 1,
    environment_id: 1,
    environment: { id: 1, name: 'production', slug: 'production', domains: [] },
    status: 'success',
    is_current: false,
    created_at: 1_700_000_000,
    url: 'https://example.com',
    ...overrides,
  } as DeploymentResponse
}

describe('filterRollbackCandidates', () => {
  test('keeps only deployments in the target environment', () => {
    const deployments = [
      makeDeployment({ id: 1, environment: { id: 1, name: 'production', slug: 'production', domains: [] } }),
      makeDeployment({ id: 2, environment: { id: 2, name: 'staging', slug: 'staging', domains: [] } }),
    ]
    expect(filterRollbackCandidates(deployments, 'production').map((d) => d.id)).toEqual([1])
  })

  test('excludes deployments that never completed successfully', () => {
    const deployments = [
      makeDeployment({ id: 1, status: 'success' }),
      makeDeployment({ id: 2, status: 'completed' }),
      makeDeployment({ id: 3, status: 'deployed' }),
      makeDeployment({ id: 4, status: 'failed' }),
      makeDeployment({ id: 5, status: 'building' }),
    ]
    expect(filterRollbackCandidates(deployments, 'production').map((d) => d.id)).toEqual([1, 2, 3])
  })

  test('caps results at 10 so the picker stays scannable', () => {
    const deployments = Array.from({ length: 15 }, (_, i) => makeDeployment({ id: i + 1 }))
    expect(filterRollbackCandidates(deployments, 'production')).toHaveLength(10)
  })
})

describe('formatRollbackChoice', () => {
  test('shows branch, short commit, and current tag for a normal deployment', () => {
    const choice = formatRollbackChoice(
      makeDeployment({ id: 42, branch: 'main', commit_hash: 'abcdef1234567', is_current: true })
    )
    expect(choice.name).toBe('#42 - main (abcdef1) (current)')
    expect(choice.value).toBe('42')
  })

  test('omits the current tag for a non-current deployment', () => {
    const choice = formatRollbackChoice(makeDeployment({ id: 7, branch: 'main', is_current: false }))
    expect(choice.name).not.toContain('(current)')
  })

  test('labels a rollback-generated deployment distinctly when branch/commit are absent', () => {
    // A deployment created BY a rollback has no branch/commit of its own —
    // falling back to "unknown"/"-" here would make the picker useless for
    // telling rollback-of-rollback chains apart.
    const choice = formatRollbackChoice(
      makeDeployment({
        id: 9,
        branch: null,
        commit_hash: null,
        metadata: { isRollback: true, rolledBackFromId: 5 },
      })
    )
    expect(choice.name).toBe('#9 - rollback (from #5)')
  })

  test('falls back to unknown/dash for a normal deployment missing branch and commit', () => {
    const choice = formatRollbackChoice(makeDeployment({ id: 3, branch: null, commit_hash: null }))
    expect(choice.name).toBe('#3 - unknown (-)')
  })
})
