// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { useState } from 'react'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { parseNumberInput } from '@/lib/parse-number-input'
import { NumberInput } from './number-input'

installBrowserWindow()
afterEach(cleanup)

function Harness({ initial }: { initial: number | undefined }) {
  const [value, setValue] = useState<number | undefined>(initial)
  return (
    <div>
      <NumberInput aria-label="limit" value={value} onValueChange={setValue} />
      <output data-testid="state">{String(value)}</output>
    </div>
  )
}

test('emptying the field leaves it empty, so typing 42 gives 42', () => {
  const view = render(<Harness initial={60} />)
  const input = view.getByLabelText('limit') as HTMLInputElement
  expect(input.value).toBe('60')

  fireEvent.change(input, { target: { value: '' } })
  expect(input.value).toBe('')
  expect(view.getByTestId('state').textContent).toBe('undefined')

  fireEvent.change(input, { target: { value: '4' } })
  fireEvent.change(input, { target: { value: '42' } })
  expect(input.value).toBe('42')
  expect(view.getByTestId('state').textContent).toBe('42')
})

test('a value changed from outside replaces the text', () => {
  function External() {
    const [value, setValue] = useState<number | undefined>(5)
    return (
      <div>
        <NumberInput
          aria-label="limit"
          value={value}
          onValueChange={setValue}
        />
        <button onClick={() => setValue(99)}>load</button>
      </div>
    )
  }
  const view = render(<External />)
  fireEvent.click(view.getByText('load'))
  expect((view.getByLabelText('limit') as HTMLInputElement).value).toBe('99')
})

test('only finite numbers are reported', () => {
  expect(parseNumberInput('')).toBeUndefined()
  expect(parseNumberInput('  ')).toBeUndefined()
  expect(parseNumberInput('abc')).toBeUndefined()
  expect(parseNumberInput('0')).toBe(0)
  expect(parseNumberInput('-3.5')).toBe(-3.5)
})
