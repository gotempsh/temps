// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'

import { Input } from '@/components/ui/input'
import {
  committedNumber,
  formatNumericText,
  isEchoOfOwnEdit,
  shouldResyncNumericText,
  validateNumeric,
  type NumericRules,
} from '@/lib/numeric-input'

type NumberInputProps = Omit<
  React.ComponentProps<typeof Input>,
  'type' | 'value' | 'defaultValue' | 'onChange'
> &
  Pick<NumericRules, 'integer'> & {
    /** The committed value; `undefined`/`null` means the field is empty. */
    value: number | null | undefined
    /**
     * Called with each valid number as it is typed, and with `undefined`
     * when the field is emptied. It is never called with a default: the
     * caller decides what an empty field means (usually a submit error).
     */
    onValueChange: (value: number | undefined) => void
    /** Mark an empty field invalid once it has been left. Default: false. */
    required?: boolean
  }

/**
 * A number field that holds its text while it is edited. Emptying it and
 * typing `42` gives 42, not a default with 42 appended. The field only flags
 * a range problem once it has been left; enforce min/max on submit with
 * `validateNumeric` / `firstNumericError` from `@/lib/numeric-input`.
 */
const NumberInput = React.forwardRef<HTMLInputElement, NumberInputProps>(
  (
    {
      value,
      onValueChange,
      min,
      max,
      integer,
      required = false,
      onBlur,
      'aria-invalid': ariaInvalid,
      ...props
    },
    ref
  ) => {
    const [text, setText] = React.useState(() => formatNumericText(value))
    const [touched, setTouched] = React.useState(false)
    // What our own onChange just emitted, cleared after the render it causes.
    // A value that arrives in that render is the parent reacting to this
    // edit: react-hook-form, for one, answers `undefined` with the field's
    // `defaultValues` entry, which would put the default straight back into a
    // field the user just emptied. A *different number* is the parent
    // overriding the edit (clamping it), which the field does follow.
    const ownEdit = React.useRef<{ emitted: number | undefined } | null>(null)

    // Follow the value when it changes from outside (a form reset, data that
    // finished loading) without disturbing what is being typed: text that
    // already means the incoming value is left alone.
    React.useEffect(() => {
      const edit = ownEdit.current
      const isEcho = edit !== null && isEchoOfOwnEdit(edit.emitted, value)
      if (shouldResyncNumericText(text, value, isEcho)) {
        setText(formatNumericText(value))
      }
      // `text` is intentionally not a dependency: only external changes sync.
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [value])

    React.useEffect(() => {
      ownEdit.current = null
    })

    const message = validateNumeric(value, {
      min: typeof min === 'number' ? min : undefined,
      max: typeof max === 'number' ? max : undefined,
      integer,
      required,
    })
    const showMessage = touched && message !== null

    // A single element on purpose: FormControl's Slot hands its id and
    // aria props to its one child.
    return (
      <Input
        {...props}
        ref={ref}
        type="number"
        min={min}
        max={max}
        value={text}
        aria-invalid={ariaInvalid ?? (showMessage ? true : undefined)}
        onChange={(event) => {
          const next = event.target.value
          setText(next)
          const committed = committedNumber(next)
          // Text that is not a number commits nothing.
          if (committed !== null) {
            ownEdit.current = { emitted: committed }
            onValueChange(committed)
          }
        }}
        onBlur={(event) => {
          setTouched(true)
          onBlur?.(event)
        }}
      />
    )
  }
)
NumberInput.displayName = 'NumberInput'

export { NumberInput }
