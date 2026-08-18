import { describe, expect, test } from 'bun:test'
import { projectCardMediaSources } from './project-card-media'

describe('project card media fallbacks', () => {
  test('tries the deployed page favicon before its screenshot', () => {
    expect(
      projectCardMediaSources(
        'https://example.temps.sh/dashboard',
        'screenshots/project.webp'
      )
    ).toEqual([
      { kind: 'favicon', src: 'https://example.temps.sh/favicon.ico' },
      {
        kind: 'screenshot',
        src: '/api/files/screenshots/project.webp',
      },
    ])
  })

  test('keeps an absolute screenshot storage path valid', () => {
    expect(projectCardMediaSources(null, '/screenshots/project.webp')).toEqual([
      {
        kind: 'screenshot',
        src: '/api/files/screenshots/project.webp',
      },
    ])
  })

  test('returns no image candidates when the project has no deployment media', () => {
    expect(projectCardMediaSources()).toEqual([])
  })

  test('ignores malformed deployment URLs but preserves the screenshot fallback', () => {
    expect(
      projectCardMediaSources('not a URL', 'screenshots/project.webp')
    ).toEqual([
      {
        kind: 'screenshot',
        src: '/api/files/screenshots/project.webp',
      },
    ])
  })
})
