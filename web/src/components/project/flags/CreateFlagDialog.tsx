// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createFlagMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { FlagValueField } from './FlagValueField'
import {
  FLAG_VALUE_TYPES,
  flagErrorMessage,
  parseFlagValue,
  validateFlagKey,
  type FlagValueType,
} from './flag-value'

interface CreateFlagDialogProps {
  projectId: number
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreated: () => void
}

const DEFAULT_FOR_TYPE: Record<FlagValueType, string> = {
  bool: 'false',
  string: '',
  number: '0',
  json: '{}',
}

export function CreateFlagDialog({
  projectId,
  open,
  onOpenChange,
  onCreated,
}: CreateFlagDialogProps) {
  const [key, setKey] = useState('')
  const [valueType, setValueType] = useState<FlagValueType>('bool')
  const [defaultValue, setDefaultValue] = useState(DEFAULT_FOR_TYPE.bool)
  const [description, setDescription] = useState('')
  const [clientVisible, setClientVisible] = useState(false)
  const [keyError, setKeyError] = useState<string | null>(null)
  const [valueError, setValueError] = useState<string | null>(null)

  const reset = () => {
    setKey('')
    setValueType('bool')
    setDefaultValue(DEFAULT_FOR_TYPE.bool)
    setDescription('')
    setClientVisible(false)
    setKeyError(null)
    setValueError(null)
  }

  const create = useMutation({
    ...createFlagMutation(),
    onSuccess: () => {
      toast.success(`Flag ${key} created`)
      reset()
      onOpenChange(false)
      onCreated()
    },
    onError: (error) => {
      toast.error(flagErrorMessage(error, 'Could not create the flag'))
    },
  })

  const handleTypeChange = (next: string) => {
    const type = next as FlagValueType
    setValueType(type)
    // Reset the default to something valid for the new type rather than
    // leaving "abc" sitting in a number field.
    setDefaultValue(DEFAULT_FOR_TYPE[type])
    setValueError(null)
  }

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault()

    const trimmedKey = key.trim()
    const keyProblem = validateFlagKey(trimmedKey)
    const parsed = parseFlagValue(defaultValue, valueType)

    setKeyError(keyProblem)
    setValueError(parsed.ok ? null : parsed.error)
    if (keyProblem || !parsed.ok) return

    create.mutate({
      path: { project_id: projectId },
      body: {
        key: trimmedKey,
        value_type: valueType,
        default_value: parsed.value,
        description: description.trim() || null,
        client_visible: clientVisible,
      },
    })
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset()
        onOpenChange(next)
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>New feature flag</DialogTitle>
            <DialogDescription>
              The key and type are permanent. Everything else can change later.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="flag-key">Key</Label>
              <Input
                id="flag-key"
                name="flag-key"
                value={key}
                onChange={(e) => {
                  setKey(e.target.value)
                  if (keyError) setKeyError(null)
                }}
                placeholder="checkout.v2"
                spellCheck={false}
                autoComplete="off"
                className={cn(
                  'font-mono text-sm',
                  keyError &&
                    'border-destructive focus-visible:ring-destructive'
                )}
              />
              {keyError ? (
                <p className="text-xs text-destructive">{keyError}</p>
              ) : (
                <p className="text-xs text-muted-foreground">
                  Lowercase letters, digits, dot, underscore and hyphen. Used
                  verbatim in your code.
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label>Type</Label>
              <RadioGroup
                value={valueType}
                onValueChange={handleTypeChange}
                className="grid grid-cols-2 gap-2 sm:grid-cols-4"
              >
                {FLAG_VALUE_TYPES.map((option) => (
                  <Label
                    key={option.value}
                    htmlFor={`flag-type-${option.value}`}
                    className={cn(
                      'flex cursor-pointer flex-col gap-1 rounded-md border p-3 transition-colors hover:bg-muted/50',
                      valueType === option.value && 'border-primary bg-muted/50'
                    )}
                  >
                    <span className="flex items-center gap-2">
                      <RadioGroupItem
                        id={`flag-type-${option.value}`}
                        value={option.value}
                      />
                      <span className="text-sm font-medium">
                        {option.label}
                      </span>
                    </span>
                    <span className="text-xs font-normal text-muted-foreground">
                      {option.hint}
                    </span>
                  </Label>
                ))}
              </RadioGroup>
            </div>

            <FlagValueField
              id="flag-default"
              label="Default value"
              type={valueType}
              value={defaultValue}
              onChange={(next) => {
                setDefaultValue(next)
                if (valueError) setValueError(null)
              }}
              error={valueError}
              description="Served whenever nothing more specific applies, and whenever the flag is disabled."
            />

            <div className="space-y-2">
              <Label htmlFor="flag-description">Description</Label>
              <Textarea
                id="flag-description"
                name="flag-description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                rows={2}
                placeholder="What this flag controls"
              />
            </div>

            <div className="flex items-start justify-between gap-4 rounded-md border p-3">
              <div className="space-y-1">
                <Label htmlFor="flag-client-visible">Expose to browsers</Label>
                <p className="text-xs text-muted-foreground">
                  Off by default. Server-only flags never reach client code.
                </p>
              </div>
              <Switch
                id="flag-client-visible"
                checked={clientVisible}
                onCheckedChange={setClientVisible}
              />
            </div>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={create.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending && (
                <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
              )}
              Create flag
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
