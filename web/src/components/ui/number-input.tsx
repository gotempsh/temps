// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'

import { Input } from '@/components/ui/input'
import { parseNumberInput } from '@/lib/parse-number-input'

type NumberInputProps = Omit<
  React.ComponentProps<'input'>,
  'type' | 'value' | 'defaultValue' | 'onChange'
> & {
  /** `undefined`, `null` and `NaN` show an empty box. */
  value: number | null | undefined
  /** Called with the parsed number, or `undefined` while the box is empty. */
  onValueChange: (value: number | undefined) => void
}

/**
 * A number field that can be emptied while it is being edited.
 *
 * Settings forms used to coerce an empty box straight back to a default
 * (`parseInt(v) || 60`, `Number(v)` -> 0), so backspacing the "60" showed "0"
 * and typing 42 produced "042". Here the box shows exactly what was typed and
 * reports `undefined` when it is empty; the form decides what an empty value
 * means (a required-field error on submit, or "use the default").
 *
 * With react-hook-form pass `onValueChange={(v) => field.onChange(v ?? NaN)}`:
 * RHF replaces an `undefined` value with the field's default, which would put
 * the old number straight back; NaN stays put and fails a `z.number()` check
 * with its message on submit.
 *
 * `min`/`max`/`step` are passed to the browser for the spinner and keyboard
 * stepping; enforcing them is the form's validation, which now only sees real
 * numbers or `undefined`.
 */
const NumberInput = React.forwardRef<HTMLInputElement, NumberInputProps>(
  ({ value, onValueChange, ...props }, ref) => {
    // NaN is how a react-hook-form field says "empty" (RHF swaps `undefined`
    // for the field's default value), so it reads as empty here too.
    const external =
      typeof value === 'number' && !Number.isNaN(value) ? value : undefined
    const [draft, setDraft] = React.useState(
      external === undefined ? '' : String(external)
    )
    const [seen, setSeen] = React.useState(external)

    // Adopt a value that changed from outside (loaded data, form reset), but
    // not the echo of what was just typed -- that would rewrite "1." to "1".
    if (external !== seen) {
      setSeen(external)
      if (parseNumberInput(draft) !== external) {
        setDraft(external === undefined ? '' : String(external))
      }
    }

    return (
      <Input
        {...props}
        ref={ref}
        type="number"
        inputMode={props.inputMode ?? 'decimal'}
        value={draft}
        onChange={(event) => {
          setDraft(event.target.value)
          onValueChange(parseNumberInput(event.target.value))
        }}
      />
    )
  }
)
NumberInput.displayName = 'NumberInput'

export { NumberInput }
