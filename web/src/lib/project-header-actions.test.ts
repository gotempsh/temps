import { describe, expect, test } from 'bun:test'
import {
  branchCommitSha,
  gitProviderFromUrl,
  gitProviderKind,
  repositoryWebUrl,
} from './project-header-actions'

describe('repositoryWebUrl', () => {
  test('removes only a trailing git clone suffix', () => {
    expect(repositoryWebUrl('https://github.com/acme/widget.git')).toBe(
      'https://github.com/acme/widget'
    )
    expect(repositoryWebUrl('https://git.example.com/acme.git/widget')).toBe(
      'https://git.example.com/acme.git/widget'
    )
  })

  test('rejects non-web schemes', () => {
    expect(repositoryWebUrl('javascript:alert(1)')).toBeNull()
    expect(repositoryWebUrl('file:///tmp/widget.git')).toBeNull()
  })

  test('turns SSH clone URLs into browser links', () => {
    expect(repositoryWebUrl('git@github.com:acme/widget.git')).toBe(
      'https://github.com/acme/widget'
    )
    expect(
      repositoryWebUrl('ssh://git@gitlab.example.com/acme/widget.git')
    ).toBe('https://gitlab.example.com/acme/widget')
  })

  test('removes credentials and token-bearing URL parts', () => {
    expect(
      repositoryWebUrl(
        'https://deploy-user:secret@github.com/acme/widget.git?token=hidden#readme'
      )
    ).toBe('https://github.com/acme/widget')
  })
})

describe('gitProviderFromUrl', () => {
  test('recognizes supported provider hosts', () => {
    expect(gitProviderFromUrl('https://github.com/acme/widget.git')).toBe(
      'github'
    )
    expect(gitProviderFromUrl('https://gitlab.com/acme/widget.git')).toBe(
      'gitlab'
    )
    expect(gitProviderFromUrl('https://bitbucket.org/acme/widget.git')).toBe(
      'bitbucket'
    )
    expect(gitProviderFromUrl('https://gitea.com/acme/widget.git')).toBe(
      'gitea'
    )
  })

  test('does not trust provider names embedded in an arbitrary hostname', () => {
    expect(gitProviderFromUrl('https://git.example.com/acme/widget.git')).toBe(
      null
    )
    expect(
      gitProviderFromUrl('https://github.evil.example/acme/widget.git')
    ).toBeNull()
    expect(
      gitProviderFromUrl('https://evilgithub.com/acme/widget.git')
    ).toBeNull()
  })
})

describe('gitProviderKind', () => {
  test('prefers the configured connection type for self-hosted providers', () => {
    expect(
      gitProviderKind('gitlab', 'https://source.example.com/acme/widget.git')
    ).toBe('gitlab')
  })

  test('falls back to the repository host for public repositories', () => {
    expect(
      gitProviderKind(undefined, 'https://bitbucket.org/acme/widget.git')
    ).toBe('bitbucket')
  })
})

describe('branchCommitSha', () => {
  test('returns the selected branch head', () => {
    expect(
      branchCommitSha(
        [
          { name: 'main', commit_sha: 'abc1234' },
          { name: 'feature', commit_sha: 'def5678' },
        ],
        'feature'
      )
    ).toBe('def5678')
  })

  test('returns null for a custom branch without provider data', () => {
    expect(
      branchCommitSha([{ name: 'main', commit_sha: 'abc1234' }], 'new')
    ).toBeNull()
  })
})
