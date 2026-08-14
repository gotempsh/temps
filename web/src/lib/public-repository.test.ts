import { describe, expect, test } from 'bun:test'
import {
  parseRepositoryCoordinates,
  parsePublicRepositoryUrl,
  publicRepositoryProvider,
} from './public-repository'

describe('public repository location', () => {
  test.each([
    ['https://github.com/example/project.git', 'github'],
    ['git@github.com:example/project.git', 'github'],
    ['https://gitlab.com/example/project', 'gitlab'],
    ['ssh://git@gitlab.com/example/project.git', 'gitlab'],
  ] as const)('detects %s as %s', (url, provider) => {
    expect(parsePublicRepositoryUrl(url)).toEqual({
      provider,
      owner: 'example',
      name: 'project',
    })
  })

  test('defaults old records without a URL to GitHub', () => {
    expect(publicRepositoryProvider(null)).toBe('github')
  })

  test('rejects unsupported hosts', () => {
    expect(
      parsePublicRepositoryUrl('https://example.test/owner/repo')
    ).toBeNull()
  })
})

describe('authenticated repository coordinates', () => {
  test('accepts owner/repository without requiring a synced record', () => {
    expect(parseRepositoryCoordinates('gotempsh/temps')).toEqual({
      owner: 'gotempsh',
      name: 'temps',
    })
  })

  test('accepts HTTPS and SSH clone references', () => {
    expect(
      parseRepositoryCoordinates('https://git.example.test/team/service.git')
    ).toEqual({ owner: 'team', name: 'service' })
    expect(
      parseRepositoryCoordinates('git@git.example.test:team/service.git')
    ).toEqual({ owner: 'team', name: 'service' })
  })

  test('rejects incomplete or ambiguous references', () => {
    expect(parseRepositoryCoordinates('service')).toBeNull()
    expect(parseRepositoryCoordinates('group/team/service')).toBeNull()
    expect(parseRepositoryCoordinates('team/my service')).toBeNull()
  })
})
