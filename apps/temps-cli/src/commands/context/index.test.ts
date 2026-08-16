import { test, expect, describe } from 'bun:test'
import { isActiveContextRow, renameFailureReason } from './index.js'

describe('isActiveContextRow', () => {
  test('with no TEMPS_CONTEXT, reflects the on-disk isActive flag', () => {
    expect(isActiveContextRow({ name: 'prod', isActive: true }, null)).toBe(true)
    expect(isActiveContextRow({ name: 'prod', isActive: false }, null)).toBe(false)
    expect(isActiveContextRow({ name: 'prod' }, null)).toBe(false)
  })

  test('TEMPS_CONTEXT overrides the on-disk flag, even for the flagged-active row', () => {
    // The whole point of the override: `local` is isActive=true on disk but
    // TEMPS_CONTEXT=prod means `prod` is what the next command actually uses.
    expect(isActiveContextRow({ name: 'local', isActive: true }, 'prod')).toBe(false)
    expect(isActiveContextRow({ name: 'prod', isActive: false }, 'prod')).toBe(true)
  })

  test('TEMPS_CONTEXT naming no configured context marks nothing active', () => {
    expect(isActiveContextRow({ name: 'prod', isActive: true }, 'staging')).toBe(false)
  })
})

describe('renameFailureReason', () => {
  const contexts = [{ name: 'prod' }, { name: 'staging' }]

  test('reports not-found when the source context is missing', () => {
    expect(renameFailureReason('typo-name', contexts)).toBe('not-found')
  })

  test('reports name-taken when the source exists (rename can only fail because the target is taken)', () => {
    expect(renameFailureReason('prod', contexts)).toBe('name-taken')
  })

  test('an empty context list is always not-found', () => {
    expect(renameFailureReason('prod', [])).toBe('not-found')
  })
})
