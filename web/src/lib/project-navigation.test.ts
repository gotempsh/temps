import { describe, expect, test } from 'bun:test'
import {
  isProjectToolsRoute,
  resolveProjectPrimaryRoute,
} from './project-navigation'

describe('project navigation route resolution', () => {
  test('keeps daily data destinations active on nested routes', () => {
    expect(resolveProjectPrimaryRoute('errors/12')).toBe('errors')
    expect(resolveProjectPrimaryRoute('traces/trace-42')).toBe('traces')
    expect(resolveProjectPrimaryRoute('deployments/99')).toBe('deployments')
    expect(resolveProjectPrimaryRoute('domains/new')).toBe('domains')
    expect(resolveProjectPrimaryRoute('git/repository')).toBe('git')
    expect(resolveProjectPrimaryRoute('build/settings')).toBe('build')
    expect(resolveProjectPrimaryRoute('settings/security')).toBe('settings')
  })

  test('keeps only the analytics overview in primary navigation', () => {
    expect(resolveProjectPrimaryRoute('analytics')).toBe('analytics')
    expect(resolveProjectPrimaryRoute('analytics/pages')).toBeNull()
    expect(resolveProjectPrimaryRoute('analytics/funnels/checkout')).toBeNull()
    expect(
      resolveProjectPrimaryRoute('analytics/visitors/visitor-42')
    ).toBeNull()
  })

  test('treats the project index as overview', () => {
    expect(resolveProjectPrimaryRoute('')).toBe('project')
    expect(resolveProjectPrimaryRoute('project')).toBe('project')
  })

  test('routes secondary capabilities through all project tools', () => {
    expect(isProjectToolsRoute('analytics/pages')).toBe(true)
    expect(isProjectToolsRoute('analytics/visitors')).toBe(true)
    expect(isProjectToolsRoute('metrics/explore')).toBe(true)
  })

  test('does not mark setup or primary routes as tools', () => {
    expect(isProjectToolsRoute('setup')).toBe(false)
    expect(isProjectToolsRoute('settings/security')).toBe(false)
    expect(isProjectToolsRoute('analytics')).toBe(false)
    expect(isProjectToolsRoute('traces/trace-1')).toBe(false)
  })
})
