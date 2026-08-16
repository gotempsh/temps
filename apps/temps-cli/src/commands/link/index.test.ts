import { test, expect, describe } from 'bun:test'
import { toProjectChoices, toEnvironmentChoices } from './index.js'

describe('toProjectChoices', () => {
  test('labels each choice with name and slug, value is the slug', () => {
    const choices = toProjectChoices([{ name: 'My App', slug: 'my-app' }])
    expect(choices).toEqual([{ name: 'My App (my-app)', value: 'my-app', description: undefined }])
  })

  test('adds a branch hint when main_branch is set', () => {
    const choices = toProjectChoices([{ name: 'My App', slug: 'my-app', main_branch: 'main' }])
    expect(choices[0]?.description).toBe('Branch: main')
  })

  test('omits the branch hint when main_branch is null or missing', () => {
    const choices = toProjectChoices([
      { name: 'A', slug: 'a', main_branch: null },
      { name: 'B', slug: 'b' },
    ])
    expect(choices[0]?.description).toBeUndefined()
    expect(choices[1]?.description).toBeUndefined()
  })

  test('empty input yields no choices', () => {
    expect(toProjectChoices([])).toEqual([])
  })
})

describe('toEnvironmentChoices', () => {
  test('name and value both come from the environment name', () => {
    const choices = toEnvironmentChoices([{ name: 'production' }])
    expect(choices).toEqual([{ name: 'production', value: 'production', description: undefined }])
  })

  test('flags preview environments, leaves others unlabeled', () => {
    const choices = toEnvironmentChoices([
      { name: 'pr-42', is_preview: true },
      { name: 'production', is_preview: false },
    ])
    expect(choices[0]?.description).toBe('Preview')
    expect(choices[1]?.description).toBeUndefined()
  })
})
