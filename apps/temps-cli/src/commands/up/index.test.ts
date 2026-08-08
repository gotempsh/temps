import { test, expect, describe } from 'bun:test'
import { resolveDefaultEnvironment, resolveExplicitEnvironment } from './index.js'

describe('resolveExplicitEnvironment', () => {
  test('resolves to the matching environment id and name', () => {
    const environments = [{ id: 1, name: 'production' }, { id: 2, name: 'staging' }]
    expect(resolveExplicitEnvironment(environments, 'staging')).toEqual({
      environmentId: 2,
      environmentName: 'staging',
    })
  })

  test('leaves environmentId undefined on a typo rather than guessing an environment', () => {
    // A silent fallback here would deploy to the wrong (or server-default)
    // environment while the preview box still claims the requested name.
    const environments = [{ id: 1, name: 'production' }]
    expect(resolveExplicitEnvironment(environments, 'prod')).toEqual({
      environmentId: undefined,
      environmentName: 'prod',
    })
  })

  test('name matching is case-sensitive, matching the API', () => {
    const environments = [{ id: 1, name: 'production' }]
    expect(resolveExplicitEnvironment(environments, 'Production').environmentId).toBeUndefined()
  })
})

describe('resolveDefaultEnvironment', () => {
  test('prefers production when present', () => {
    const environments = [{ id: 1, name: 'staging' }, { id: 2, name: 'production' }]
    expect(resolveDefaultEnvironment(environments)).toEqual({
      environmentId: 2,
      environmentName: 'production',
    })
  })

  test('falls back to the first environment when there is no production', () => {
    const environments = [{ id: 5, name: 'staging' }, { id: 6, name: 'qa' }]
    expect(resolveDefaultEnvironment(environments)).toEqual({
      environmentId: 5,
      environmentName: 'staging',
    })
  })

  test('falls back to a bare "production" label when there are no environments at all', () => {
    expect(resolveDefaultEnvironment([])).toEqual({
      environmentId: undefined,
      environmentName: 'production',
    })
  })
})
