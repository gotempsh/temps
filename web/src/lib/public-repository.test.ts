import { describe, expect, test } from 'bun:test'
import {
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
