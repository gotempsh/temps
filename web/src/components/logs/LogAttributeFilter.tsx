// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { Button, Field, FormErrors } from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import type { LogAttributeOperator, LogAttributeType } from '@/api/client'

export interface LogAttributePredicate {
  key: string
  value: string
  type?: LogAttributeType
  operator: LogAttributeOperator
}
const ANY = '__any__'
const TYPES: LogAttributeType[] = [
  'string',
  'boolean',
  'integer',
  'double',
  'array',
  'object',
  'bytes',
]
const OPERATORS: { value: LogAttributeOperator; label: string }[] = [
  { value: 'eq', label: 'Equals' },
  { value: 'ne', label: 'Does not equal' },
  { value: 'gt', label: 'Greater than' },
  { value: 'gte', label: 'At least' },
  { value: 'lt', label: 'Less than' },
  { value: 'lte', label: 'At most' },
]

const attributeFilterSchema = z
  .object({
    key: z
      .string()
      .refine(
        (value) =>
          value.length > 0 &&
          new TextEncoder().encode(value).length <= 128 &&
          !value.includes('\0'),
        'Enter an attribute key of 1–128 bytes.'
      ),
    text: z
      .string()
      .refine(
        (value) =>
          new TextEncoder().encode(value).length <= 16384 &&
          !value.includes('\0'),
        'Enter a value of at most 16 KiB without NUL characters.'
      ),
    type: z.enum([
      ANY,
      'string',
      'boolean',
      'integer',
      'double',
      'array',
      'object',
      'bytes',
    ]),
    operator: z.enum(['eq', 'ne', 'gt', 'gte', 'lt', 'lte']),
  })
  .superRefine(({ type, text, operator }, context) => {
    let message: string | undefined
    if (type === 'integer') {
      try {
        if (
          !/^[+-]?\d+$/.test(text) ||
          BigInt(text) < -(2n ** 63n) ||
          BigInt(text) >= 2n ** 63n
        )
          message = 'Enter a signed 64-bit integer.'
      } catch {
        message = 'Enter a signed 64-bit integer.'
      }
    }
    if (
      type === 'double' &&
      (!/^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/.test(text) ||
        !Number.isFinite(Number(text)))
    )
      message = 'Enter a finite number.'
    if (type === 'boolean' && text !== 'true' && text !== 'false')
      message = 'Enter true or false.'
    if (message) context.addIssue({ code: 'custom', path: ['text'], message })
    if (
      type !== 'integer' &&
      type !== 'double' &&
      operator !== 'eq' &&
      operator !== 'ne'
    )
      context.addIssue({
        code: 'custom',
        path: ['operator'],
        message: 'Range comparisons require a numeric type.',
      })
  })
type AttributeFilterValues = z.infer<typeof attributeFilterSchema>

/** Form section inside the existing log explorer; requests use its native SDK. */
export function LogAttributeFilter({
  value,
  onApply,
  pending,
}: {
  value?: LogAttributePredicate
  onApply: (value?: LogAttributePredicate) => void
  pending: boolean
}) {
  const form = useForm<AttributeFilterValues>({
    resolver: zodResolver(attributeFilterSchema),
    defaultValues: {
      key: value?.key ?? '',
      text: value?.value ?? '',
      type: value?.type ?? ANY,
      operator: value?.operator ?? 'eq',
    },
  })
  const type = useWatch({ control: form.control, name: 'type' })
  const operator = useWatch({ control: form.control, name: 'operator' })
  const errors = {
    Attribute: form.formState.errors.key?.message,
    Value: form.formState.errors.text?.message,
    Comparison: form.formState.errors.operator?.message,
  }
  const numeric = type === 'integer' || type === 'double'
  const submit = form.handleSubmit(({ key, text, type, operator }) => {
    // Preserve int64 precision across the native API boundary.
    onApply({
      key,
      value: text,
      type: type === ANY ? undefined : type,
      operator,
    })
  })
  return (
    <form
      className="mt-3 space-y-3 border-t pt-3"
      onSubmit={submit}
      aria-label="Filter log attributes"
    >
      <p className="text-xs text-muted-foreground">
        Filter an attribute by value and declared type. Older logs without
        native type metadata can use “Any type”.
      </p>
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <Field label="Attribute key" error={errors.Attribute}>
          {(props) => (
            <Input
              {...props}
              {...form.register('key')}
              placeholder="order.total"
            />
          )}
        </Field>
        <Field label="Attribute type" optional>
          {(props) => (
            <Select
              value={type}
              onValueChange={(value) => {
                form.setValue('type', value as AttributeFilterValues['type'])
                if (value !== 'integer' && value !== 'double')
                  form.setValue('operator', 'eq')
              }}
            >
              <SelectTrigger {...props}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ANY}>Any type</SelectItem>
                {TYPES.map((value) => (
                  <SelectItem key={value} value={value}>
                    {value}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </Field>
        <Field label="Comparison">
          {(props) => (
            <Select
              value={operator}
              onValueChange={(value) =>
                form.setValue('operator', value as LogAttributeOperator)
              }
            >
              <SelectTrigger {...props}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {OPERATORS.filter(
                  (item) =>
                    numeric || item.value === 'eq' || item.value === 'ne'
                ).map((item) => (
                  <SelectItem key={item.value} value={item.value}>
                    {item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </Field>
        <Field label="Attribute value" error={errors.Value}>
          {(props) => (
            <Input
              {...props}
              {...form.register('text')}
              placeholder={type === 'boolean' ? 'true' : 'Value'}
            />
          )}
        </Field>
      </div>
      <FormErrors errors={errors} />
      <div className="flex flex-wrap gap-2">
        <Button
          type="submit"
          variant="outline"
          busy={pending}
          busyLabel="Applying…"
        >
          Apply attribute filter
        </Button>
        {value && (
          <Button
            type="button"
            variant="ghost"
            onClick={() => {
              form.reset({ key: '', text: '', type: ANY, operator: 'eq' })
              onApply(undefined)
            }}
          >
            Clear attribute filter
          </Button>
        )}
      </div>
    </form>
  )
}
