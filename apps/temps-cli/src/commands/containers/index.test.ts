import { test, expect, describe } from 'bun:test'
import { containerStatusVariant, createProgressBar, formatBytes } from './index.js'

describe('containerStatusVariant', () => {
  test('maps a running status to active', () => {
    expect(containerStatusVariant('running')).toBe('active')
  })

  test('is case-insensitive', () => {
    expect(containerStatusVariant('Running')).toBe('active')
  })

  test('matches "running" as a substring, e.g. Docker\'s "Up 3 minutes (running)"', () => {
    expect(containerStatusVariant('Up 3 minutes (running)')).toBe('active')
  })

  test('maps stopped/exited/anything else to inactive', () => {
    expect(containerStatusVariant('stopped')).toBe('inactive')
    expect(containerStatusVariant('exited (0)')).toBe('inactive')
    expect(containerStatusVariant('')).toBe('inactive')
  })
})

describe('formatBytes', () => {
  test('renders sub-1024 values in bytes', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(0)).toBe('0 B')
  })

  test('scales to KB, MB, and GB at the right thresholds', () => {
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(1024 * 1024 * 3)).toBe('3.0 MB')
    expect(formatBytes(1024 * 1024 * 1024 * 1.5)).toBe('1.50 GB')
  })

  test('is exclusive at the 1024 boundary, not inclusive', () => {
    // 1024 itself should already read as KB, not "1024 B".
    expect(formatBytes(1024)).toBe('1.0 KB')
  })
})

describe('createProgressBar', () => {
  test('fills proportionally to percent, at the given width', () => {
    const bar = createProgressBar(50, 10)
    expect(bar).toBe('█████░░░░░')
  })

  test('is fully filled at 100%', () => {
    expect(createProgressBar(100, 10)).toBe('█'.repeat(10))
  })

  test('is fully empty at 0%', () => {
    expect(createProgressBar(0, 10)).toBe('░'.repeat(10))
  })

  test('defaults to a width of 30', () => {
    expect(createProgressBar(0).length).toBe(30)
  })
})
