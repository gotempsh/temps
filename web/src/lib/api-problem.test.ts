import { describe, expect, test } from 'bun:test'
import { gitProviderSetupPath } from './api-problem'

describe('gitProviderSetupPath', () => {
  test('accepts the repository connection route from a rate limit problem', () => {
    expect(
      gitProviderSetupPath({
        setup_path: '/projects/example-app/git/change-repository',
      })
    ).toBe('/projects/example-app/git/change-repository')
  })

  test('rejects external or unrelated routes', () => {
    expect(gitProviderSetupPath({ setup_path: 'https://evil.example' })).toBe(
      undefined
    )
    expect(gitProviderSetupPath({ setup_path: '//evil.example' })).toBe(
      undefined
    )
    expect(gitProviderSetupPath({ setup_path: '/settings/nodes' })).toBe(
      undefined
    )
  })
})
