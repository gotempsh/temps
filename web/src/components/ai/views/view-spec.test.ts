// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { VIEW_LIMITS } from './limits'
import { parseViewSpec } from './view-spec'

const add = (path: string, value: unknown) =>
  JSON.stringify({ op: 'add', path, value })
const el = (
  key: string,
  type: string,
  props: unknown,
  children: string[] = []
) => add(`/elements/${key}`, { type, props, children })
const lines = (...written: string[]) => written.join('\n')

const STAT = { label: 'Running', value: '3', tone: 'ok' }

describe('parseViewSpec', () => {
  test('reads the patch lines a model writes', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'main'),
        el('main', 'Stats', {}, ['running']),
        el('running', 'Stat', STAT)
      )
    )
    expect(view).toEqual({
      spec: {
        root: 'main',
        elements: {
          main: { type: 'Stats', props: {}, children: ['running'] },
          running: { type: 'Stat', props: STAT, children: [] },
        },
      },
      dropped: 0,
    })
  })

  test('also reads one whole spec object', () => {
    const view = parseViewSpec(
      JSON.stringify({
        root: 'state',
        elements: {
          state: { type: 'Status', props: { tone: 'ok', label: 'Running' } },
        },
      })
    )
    expect(view?.spec.elements.state).toEqual({
      type: 'Status',
      props: { tone: 'ok', label: 'Running' },
      children: [],
    })
  })

  test('describes nothing when the block is empty, prose or has no root', () => {
    expect(parseViewSpec('')).toBeNull()
    expect(parseViewSpec('here is your view')).toBeNull()
    expect(parseViewSpec(el('running', 'Stat', STAT))).toBeNull()
    expect(parseViewSpec(add('/root', 'missing'))).toBeNull()
  })

  test('draws what has arrived while the rest is still being written', () => {
    const whole = lines(
      add('/root', 'main'),
      el('main', 'Stats', {}, ['running', 'failing']),
      el('running', 'Stat', STAT),
      el('failing', 'Stat', { label: 'Failing', value: '1' })
    )
    // Cut in the middle of the last line.
    const view = parseViewSpec(whole.slice(0, whole.length - 20))
    expect(Object.keys(view?.spec.elements ?? {})).toEqual(['main', 'running'])
    expect(view?.spec.elements.main.children).toEqual(['running'])
    // The cut line, and the child it would have been.
    expect(view?.dropped).toBe(2)
  })

  test('a table is read one row per line, and a bad line costs only its row', () => {
    const row = (value: unknown) => add('/elements/pages/props/rows/-', value)
    const view = parseViewSpec(
      lines(
        add('/root', 'pages'),
        el('pages', 'Table', { columns: ['Page', 'Views'], rows: [] }),
        row(['/', '803']),
        // What a model writes when it loses track of its brackets.
        '{"op":"add","path":"/elements/pages/props/rows/-","value":["/products"],"337"]}',
        row([{ text: '/cart', href: '/projects/web' }, 117])
      )
    )
    expect(view?.spec.elements.pages.props).toEqual({
      columns: ['Page', 'Views'],
      rows: [
        ['/', '803'],
        [{ text: '/cart', href: '/projects/web' }, '117'],
      ],
    })
    // The lost row is counted, so the view can say it is not whole.
    expect(view?.dropped).toBe(1)
  })

  test('a component the console does not have is left out and counted', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'main'),
        el('main', 'Stack', {}, ['frame', 'proto', 'state']),
        el('frame', 'Iframe', { src: 'https://elsewhere.example' }),
        el('proto', 'constructor', {}),
        el('state', 'Status', { tone: 'ok', label: 'Running' })
      )
    )
    expect(view?.spec.elements.main.children).toEqual(['state'])
    expect(Object.keys(view?.spec.elements ?? {})).toEqual(['main', 'state'])
    expect(view?.dropped).toBe(2)
  })

  test('props that do not fit the component drop the element', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'main'),
        el('main', 'Stack', {}, ['bad', 'good']),
        el('bad', 'Status', { tone: 'purple', label: 'Odd' }),
        el('good', 'Status', { tone: 'warn', label: 'Degraded' })
      )
    )
    expect(view?.spec.elements.main.children).toEqual(['good'])
    expect(view?.dropped).toBe(1)
  })

  test('only type, known props and children survive', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'main'),
        add('/state', { secret: 'x' }),
        add('/elements/main', {
          type: 'Status',
          props: { tone: 'ok', label: 'Running', onClick: 'alert(1)' },
          children: ['main'],
          on: { press: { action: 'navigate' } },
          visible: { $state: '/secret' },
          repeat: { statePath: '/items' },
        })
      )
    )
    expect(view?.spec).toEqual({
      root: 'main',
      elements: {
        main: {
          type: 'Status',
          props: { tone: 'ok', label: 'Running' },
          // A Status takes no children, so none are read.
          children: [],
        },
      },
    })
  })

  test('numbers become text and a long text is cut, not lost', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'table'),
        el('table', 'Table', {
          columns: ['Service', 'Replicas', 'Last error'],
          rows: [['web', 2, 'x'.repeat(VIEW_LIMITS.text + 50)]],
        })
      )
    )
    const rows = (view?.spec.elements.table.props as { rows: string[][] }).rows
    expect(rows[0][1]).toBe('2')
    expect(rows[0][2]).toHaveLength(VIEW_LIMITS.text)
    expect(rows[0][2].endsWith('…')).toBe(true)
    expect(view?.dropped).toBe(0)
  })

  test('a table past its limits is not drawn', () => {
    const rows = Array.from({ length: VIEW_LIMITS.rows + 1 }, () => ['web'])
    expect(
      parseViewSpec(
        lines(
          add('/root', 'table'),
          el('table', 'Table', { columns: ['Service'], rows })
        )
      )
    ).toBeNull()
  })

  test('an element is reached once: no cycles and no shared subtrees', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'a'),
        el('a', 'Stack', {}, ['b', 'shared', 'shared']),
        el('b', 'Stack', {}, ['a', 'shared']),
        el('shared', 'Status', { tone: 'idle', label: 'Stopped' })
      )
    )
    expect(view?.spec.elements.a.children).toEqual(['b'])
    expect(view?.spec.elements.b.children).toEqual(['shared'])
    // `a` again under `b`, and `shared` twice more under `a`.
    expect(view?.dropped).toBe(3)
  })

  test('a view stops at its element limit', () => {
    const keys = Array.from(
      { length: VIEW_LIMITS.elements + 20 },
      (_, index) => `s${index}`
    )
    const view = parseViewSpec(
      lines(
        add('/root', 'main'),
        el('main', 'Stack', {}, keys),
        ...keys.map((key) => el(key, 'Status', { tone: 'ok', label: key }))
      )
    )
    expect(Object.keys(view?.spec.elements ?? {})).toHaveLength(
      VIEW_LIMITS.elements
    )
    expect(view?.dropped).toBe(21)
  })

  test('a chart keeps the numbers it draws from, and reads a quoted one', () => {
    const view = parseViewSpec(
      lines(
        add('/root', 'main'),
        el('main', 'Stack', {}, ['daily', 'channels']),
        el('daily', 'Trend', { label: 'Visitors', points: [], partial: true }),
        add('/elements/daily/props/points/-', { at: 'Nov 24', value: 112 }),
        add('/elements/daily/props/points/-', { at: 25, value: '181' }),
        el('channels', 'Bars', {
          items: [
            { label: 'Direct', value: 2304, text: '44.9%' },
            { label: 'Email', value: 257.5 },
          ],
        })
      )
    )
    expect(view?.spec.elements.daily.props).toEqual({
      label: 'Visitors',
      // A label written as a number is still text; a value is still a number.
      points: [
        { at: 'Nov 24', value: 112 },
        { at: '25', value: 181 },
      ],
      partial: true,
    })
    expect(view?.spec.elements.channels.props).toEqual({
      items: [
        { label: 'Direct', value: 2304, text: '44.9%' },
        { label: 'Email', value: 257.5 },
      ],
    })
    expect(view?.dropped).toBe(0)
  })

  test('a chart is not drawn from text that only looks like a number', () => {
    for (const value of ['1,829', '39%', '', null, 'many', -3]) {
      const view = parseViewSpec(
        lines(
          add('/root', 'bars'),
          el('bars', 'Bars', { items: [{ label: 'Direct', value }] })
        )
      )
      expect(view).toBeNull()
    }
    // A trend needs two points to be a line.
    expect(
      parseViewSpec(
        lines(
          add('/root', 'trend'),
          el('trend', 'Trend', {
            label: 'Visitors',
            points: [{ at: 'Nov 25', value: 181 }],
          })
        )
      )
    ).toBeNull()
  })
})
