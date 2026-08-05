import { describe, expect, test } from 'bun:test'
import { consumeDropFilesHandoff, handOffDropFiles } from './drop-handoff'

describe('drop file handoff', () => {
  test('transfers files exactly once without putting them in browser history', () => {
    const file = new File(['hello'], 'index.html', { type: 'text/html' })
    handOffDropFiles([{ file, path: 'site/index.html' }])

    expect(consumeDropFilesHandoff()).toEqual([
      { file, path: 'site/index.html' },
    ])
    expect(consumeDropFilesHandoff()).toBeNull()
  })
})
