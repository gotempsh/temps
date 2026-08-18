import { describe, expect, test } from 'bun:test'
import type { GettingStartedItem } from '@/hooks/useGettingStarted'
import {
  firstIncompleteGettingStartedIndex,
  nextIncompleteGettingStartedItem,
} from './onboarding-next-step'

function step(key: string, done: boolean): GettingStartedItem {
  return {
    key,
    label: key,
    description: `${key} description`,
    done,
    href: `/${key}`,
    cta: `Open ${key}`,
  }
}

describe('dashboard onboarding next step', () => {
  test('shows the first incomplete step in checklist order', () => {
    expect(
      nextIncompleteGettingStartedItem([
        step('ai', true),
        step('git', false),
        step('domain', false),
      ])?.key
    ).toBe('git')
  })

  test('returns no step when setup is complete', () => {
    expect(
      nextIncompleteGettingStartedItem([step('ai', true), step('git', true)])
    ).toBeUndefined()
  })

  test('returns the first incomplete step index for the dashboard navigator', () => {
    expect(
      firstIncompleteGettingStartedIndex([
        step('ai', true),
        step('git', true),
        step('domain', false),
      ])
    ).toBe(2)
  })

  test('returns -1 when every setup step is complete', () => {
    expect(
      firstIncompleteGettingStartedIndex([step('ai', true), step('git', true)])
    ).toBe(-1)
  })
})
