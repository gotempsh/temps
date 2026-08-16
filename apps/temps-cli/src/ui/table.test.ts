import { describe, expect, test } from 'bun:test'
import { createTable } from './table.js'

describe('createTable', () => {
  test('sanitizes headers and values before terminal rendering', () => {
    const rendered = createTable(
      [{ name: '\x1b]52;c;dG9rX2xpdmVfc2VjcmV0\x07safe\nrow' }],
      [{ header: '\x1b[31mName\x1b[0m', key: 'name' }],
      { style: 'minimal' },
    )

    expect(rendered).toContain('Name')
    expect(rendered).toContain('safe row')
    expect(rendered).not.toContain(']52;')
    expect(rendered).not.toContain('\x1b[31m')
  })

  test('applies trusted column styling after sanitizing accessor output', () => {
    const rendered = createTable(
      [{ status: '\x1b]52;c;Zm9yZ2Vk\x07inactive' }],
      [{ header: 'Status', accessor: (row) => row.status, color: (value) => `\x1b[33m${value}\x1b[0m` }],
      { style: 'minimal' },
    )

    expect(rendered).toContain('\x1b[33minactive\x1b[0m')
    expect(rendered).not.toContain(']52;')
  })
})
