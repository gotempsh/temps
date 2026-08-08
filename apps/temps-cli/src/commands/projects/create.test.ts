import { test, expect, describe } from 'bun:test'
import { parseRepoPath } from './create.js'

describe('parseRepoPath', () => {
  test('splits a simple owner/name pair', () => {
    expect(parseRepoPath('myorg/myrepo')).toEqual({ owner: 'myorg', name: 'myrepo' })
  })

  test('splits on the LAST slash so nested GitLab groups keep their full owner path', () => {
    // acme/infra/platform/my-repo -> owner "acme/infra/platform", not just "infra".
    expect(parseRepoPath('acme/infra/platform/my-repo')).toEqual({
      owner: 'acme/infra/platform',
      name: 'my-repo',
    })
  })

  test('rejects a string with no slash', () => {
    expect(parseRepoPath('myrepo')).toBeNull()
  })

  test('rejects a leading slash (empty owner)', () => {
    expect(parseRepoPath('/myrepo')).toBeNull()
  })

  test('rejects a trailing slash (empty name)', () => {
    expect(parseRepoPath('myorg/')).toBeNull()
  })
})
