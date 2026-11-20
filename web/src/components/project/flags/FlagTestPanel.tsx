// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, FlagResponse } from '@/api/client'
import { evaluateFlagMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { describeReason, parseAttributeLines } from './flag-rules'
import {
  asFlagValueType,
  flagErrorMessage,
  formatFlagValue,
} from './flag-value'

/**
 * "Who would see what": evaluates the flag on the server, with the same
 * evaluator apps use, for a subject and attributes typed here. Read-only.
 */
export function FlagTestPanel({
  projectId,
  flag,
  environments,
}: {
  projectId: number
  flag: FlagResponse
  environments: EnvironmentResponse[]
}) {
  const valueType = asFlagValueType(flag.value_type)
  const [environmentId, setEnvironmentId] = useState<string>(
    environments[0] ? String(environments[0].id) : ''
  )
  const [subject, setSubject] = useState('')
  const [attributes, setAttributes] = useState('')
  const [inputError, setInputError] = useState<string | null>(null)

  const evaluate = useMutation({ ...evaluateFlagMutation() })
  // A result belongs to the inputs it was computed for. Changing any input
  // clears it, and `reset` also detaches a request still in flight, so its
  // answer never appears under different inputs.
  const changed =
    <T,>(set: (value: T) => void) =>
    (value: T) => {
      set(value)
      evaluate.reset()
    }

  const run = () => {
    const parsed = parseAttributeLines(attributes)
    if (!parsed.ok) {
      setInputError(parsed.error)
      return
    }
    if (!environmentId) {
      setInputError('Choose an environment.')
      return
    }
    setInputError(null)
    evaluate.mutate({
      path: { project_id: projectId, key: flag.key },
      body: {
        environment_id: Number(environmentId),
        key: subject.trim() || null,
        attributes: parsed.attributes,
      },
    })
  }

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="flag-test-environment">Environment</Label>
          <Select
            value={environmentId}
            onValueChange={changed(setEnvironmentId)}
          >
            <SelectTrigger id="flag-test-environment" className="w-full">
              <SelectValue placeholder="Choose an environment" />
            </SelectTrigger>
            <SelectContent>
              {environments.map((environment) => (
                <SelectItem key={environment.id} value={String(environment.id)}>
                  {environment.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-2">
          <Label htmlFor="flag-test-subject">Subject key</Label>
          <Input
            id="flag-test-subject"
            value={subject}
            onChange={(e) => changed(setSubject)(e.target.value)}
            placeholder="user_1001"
            spellCheck={false}
          />
        </div>
      </div>
      <div className="space-y-2">
        <Label htmlFor="flag-test-attributes">Attributes</Label>
        <Textarea
          id="flag-test-attributes"
          value={attributes}
          onChange={(e) => changed(setAttributes)(e.target.value)}
          rows={3}
          spellCheck={false}
          placeholder={'plan=pro\ncountry=ES'}
          className="font-mono text-xs"
        />
        <p className="text-xs text-muted-foreground">
          One <code>name=value</code> per line. Browsers also send country,
          browser, os and device_type automatically.
        </p>
      </div>

      {inputError && <p className="text-xs text-destructive">{inputError}</p>}

      <Button
        type="button"
        size="sm"
        variant="outline"
        onClick={run}
        disabled={evaluate.isPending || environments.length === 0}
      >
        {evaluate.isPending && (
          <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
        )}
        Evaluate
      </Button>

      {evaluate.isError && (
        <p className="text-xs text-destructive">
          {flagErrorMessage(evaluate.error, 'Could not evaluate the flag')}
        </p>
      )}

      {evaluate.data && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 rounded-md border p-3 text-sm">
          <dt className="text-muted-foreground">Value</dt>
          <dd className="break-all font-mono">
            {formatFlagValue(evaluate.data.value, valueType)}
          </dd>
          <dt className="text-muted-foreground">Variant</dt>
          <dd className="font-mono">{evaluate.data.variant ?? '—'}</dd>
          <dt className="text-muted-foreground">Why</dt>
          <dd>{describeReason(evaluate.data.reason)}</dd>
        </dl>
      )}
    </div>
  )
}
