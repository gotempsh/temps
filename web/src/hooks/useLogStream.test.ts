import { describe, expect, test } from 'bun:test'
import { buildLogStreamUrl } from './useLogStream'

describe('buildLogStreamUrl', () => {
  test('requests a bounded Docker backlog for restart-heavy containers', () => {
    const url = new URL(
      buildLogStreamUrl(
        'ws://localhost/api/projects/1/environments/2/containers/abc/logs',
        'http://localhost',
        true,
        1000
      )
    )

    expect(url.searchParams.get('tail')).toBe('1000')
    expect(url.searchParams.get('timestamps')).toBe('true')
  })

  test('preserves existing filters while setting the effective tail', () => {
    const url = new URL(
      buildLogStreamUrl(
        '/api/logs?start_date=123&tail=all',
        'http://localhost:3013',
        false,
        500
      )
    )

    expect(url.searchParams.get('start_date')).toBe('123')
    expect(url.searchParams.get('tail')).toBe('500')
    expect(url.searchParams.get('timestamps')).toBe('false')
  })
})
