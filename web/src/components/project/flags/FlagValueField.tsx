// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import type { FlagValueType } from './flag-value'

interface FlagValueFieldProps {
  id: string
  label: string
  type: FlagValueType
  /** Raw text for string/number/json; `'true'`/`'false'` for bool. */
  value: string
  onChange: (next: string) => void
  error?: string | null
  description?: string
  disabled?: boolean
}

/**
 * One editable flag value, shaped by the flag's declared type.
 *
 * A boolean gets a switch, not a text box that accepts the word "true" — the
 * type is known, so the control should be too.
 */
export function FlagValueField({
  id,
  label,
  type,
  value,
  onChange,
  error,
  description,
  disabled,
}: FlagValueFieldProps) {
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>

      {type === 'bool' ? (
        <div className="flex items-center gap-3">
          <Switch
            id={id}
            checked={value === 'true'}
            onCheckedChange={(checked) => onChange(checked ? 'true' : 'false')}
            disabled={disabled}
          />
          <span className="text-sm tabular-nums">
            {value === 'true' ? 'On' : 'Off'}
          </span>
        </div>
      ) : type === 'json' ? (
        <Textarea
          id={id}
          name={id}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          disabled={disabled}
          rows={6}
          spellCheck={false}
          placeholder={'{\n  "key": "value"\n}'}
          className={cn(
            'font-mono text-xs',
            error && 'border-destructive focus-visible:ring-destructive'
          )}
        />
      ) : (
        <Input
          id={id}
          name={id}
          type={type === 'number' ? 'number' : 'text'}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          disabled={disabled}
          spellCheck={false}
          placeholder={type === 'number' ? '0' : 'value'}
          className={cn(
            type === 'number' && 'tabular-nums',
            error && 'border-destructive focus-visible:ring-destructive'
          )}
        />
      )}

      {error ? (
        <p className="text-xs text-destructive">{error}</p>
      ) : description ? (
        <p className="text-xs text-muted-foreground">{description}</p>
      ) : null}
    </div>
  )
}
