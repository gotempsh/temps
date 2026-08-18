import { describe, expect, test } from 'bun:test'
import { projectHasGitRepo } from './project-git'

describe('projectHasGitRepo', () => {
  test('treats the "unknown" placeholder as no repository', () => {
    // What project creation writes when no repo was supplied — the case that
    // produced a 404 branch fetch against github.com/unknown/unknown.
    expect(
      projectHasGitRepo({ repo_owner: 'unknown', repo_name: 'unknown' })
    ).toBe(false)
  })

  test('treats empty and whitespace-only fields as no repository', () => {
    expect(projectHasGitRepo({ repo_owner: '', repo_name: '' })).toBe(false)
    expect(projectHasGitRepo({ repo_owner: '  ', repo_name: '  ' })).toBe(false)
    expect(projectHasGitRepo({})).toBe(false)
    expect(projectHasGitRepo({ repo_owner: null, repo_name: null })).toBe(false)
  })

  test('requires both owner and name for a public repo', () => {
    expect(projectHasGitRepo({ repo_owner: 'gotempsh', repo_name: '' })).toBe(
      false
    )
    expect(
      projectHasGitRepo({ repo_owner: 'unknown', repo_name: 'temps' })
    ).toBe(false)
    expect(
      projectHasGitRepo({ repo_owner: 'gotempsh', repo_name: 'temps' })
    ).toBe(true)
  })

  test('a provider connection counts even before a repo is picked', () => {
    // Connected-but-not-yet-configured is an onboarding state; the section
    // must stay visible so the user can finish setting it up.
    expect(
      projectHasGitRepo({
        repo_owner: 'unknown',
        repo_name: 'unknown',
        git_provider_connection_id: 7,
      })
    ).toBe(true)
  })

  test('a connection id of 0 is not a connection', () => {
    expect(
      projectHasGitRepo({
        repo_owner: 'unknown',
        repo_name: 'unknown',
        git_provider_connection_id: 0,
      })
    ).toBe(false)
  })

  test('an explicit git url counts on its own', () => {
    expect(
      projectHasGitRepo({
        repo_owner: 'unknown',
        repo_name: 'unknown',
        git_url: 'https://git.example.com/team/app.git',
      })
    ).toBe(true)
  })
})
