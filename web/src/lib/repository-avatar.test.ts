import { describe, expect, test } from 'bun:test'
import { repositoryAvatarUrl } from './repository-avatar'

describe('repositoryAvatarUrl', () => {
  test('uses the GitHub owner avatar for public repositories', () => {
    expect(
      repositoryAvatarUrl(
        {
          owner: 'gotempsh',
          clone_url: 'https://github.com/gotempsh/temps.git',
        },
        'github'
      )
    ).toBe('https://github.com/gotempsh.png?size=64')
  })

  test('keeps GitHub Enterprise avatars on the connected host', () => {
    expect(
      repositoryAvatarUrl(
        {
          owner: 'platform',
          clone_url: 'https://git.example.com/platform/app.git',
        },
        'github'
      )
    ).toBe('https://git.example.com/platform.png?size=64')
  })

  test('uses the neutral fallback when no stable provider avatar exists', () => {
    expect(
      repositoryAvatarUrl(
        {
          owner: 'group',
          clone_url: 'https://gitlab.com/group/app.git',
        },
        'gitlab'
      )
    ).toBeNull()
  })

  test('rejects nested or empty owners instead of constructing an invalid URL', () => {
    expect(
      repositoryAvatarUrl({ owner: 'group/team', clone_url: null }, 'github')
    ).toBeNull()
    expect(
      repositoryAvatarUrl({ owner: ' ', clone_url: null }, 'github')
    ).toBeNull()
  })
})
