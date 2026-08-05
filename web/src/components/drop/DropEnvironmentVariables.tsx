import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import type { DropEnvironmentVariable } from '@/lib/drop-environment-variables'
import { Eye, EyeOff, KeyRound, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'

let nextVariableId = 0

function createVariableId(): string {
  nextVariableId += 1
  return `drop-env-${nextVariableId}`
}

export function DropEnvironmentVariables({
  variables,
  onChange,
  disabled = false,
}: {
  variables: DropEnvironmentVariable[]
  onChange: (variables: DropEnvironmentVariable[]) => void
  disabled?: boolean
}) {
  const [visibleValues, setVisibleValues] = useState<Set<string>>(new Set())

  const update = (id: string, field: 'key' | 'value', value: string) => {
    onChange(
      variables.map((variable) =>
        variable.id === id ? { ...variable, [field]: value } : variable
      )
    )
  }

  const add = () => {
    onChange([...variables, { id: createVariableId(), key: '', value: '' }])
  }

  const remove = (id: string) => {
    onChange(variables.filter((variable) => variable.id !== id))
    setVisibleValues((current) => {
      const next = new Set(current)
      next.delete(id)
      return next
    })
  }

  const toggleVisible = (id: string) => {
    setVisibleValues((current) => {
      const next = new Set(current)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  return (
    <section className="space-y-3 rounded-xl border bg-muted/20 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-start gap-2.5">
          <KeyRound className="mt-0.5 size-4 text-muted-foreground" />
          <div>
            <p className="text-sm font-medium">Environment variables</p>
            <p className="mt-0.5 text-xs leading-5 text-muted-foreground">
              Optional values available to the first deployment.
            </p>
          </div>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={add}
          disabled={disabled}
        >
          <Plus className="mr-1.5 size-3.5" /> Add variable
        </Button>
      </div>

      {variables.length > 0 && (
        <div className="space-y-3 border-t pt-3">
          {variables.map((variable, index) => {
            const valueVisible = visibleValues.has(variable.id)
            return (
              <div
                key={variable.id}
                className="grid gap-2 sm:grid-cols-[minmax(0,0.8fr)_minmax(0,1.2fr)_auto] sm:items-end"
              >
                <div className="space-y-1.5">
                  <Label htmlFor={`drop-env-key-${variable.id}`}>
                    {index === 0 ? 'Key' : <span className="sr-only">Key</span>}
                  </Label>
                  <Input
                    id={`drop-env-key-${variable.id}`}
                    value={variable.key}
                    placeholder="API_URL"
                    autoComplete="off"
                    spellCheck={false}
                    disabled={disabled}
                    onChange={(event) =>
                      update(variable.id, 'key', event.target.value)
                    }
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor={`drop-env-value-${variable.id}`}>
                    {index === 0 ? (
                      'Value'
                    ) : (
                      <span className="sr-only">Value</span>
                    )}
                  </Label>
                  <div className="relative">
                    <Input
                      id={`drop-env-value-${variable.id}`}
                      type={valueVisible ? 'text' : 'password'}
                      value={variable.value}
                      placeholder="Enter value"
                      autoComplete="new-password"
                      disabled={disabled}
                      className="pr-10"
                      onChange={(event) =>
                        update(variable.id, 'value', event.target.value)
                      }
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="absolute right-0 top-0 size-9"
                      onClick={() => toggleVisible(variable.id)}
                      disabled={disabled}
                      aria-label={valueVisible ? 'Hide value' : 'Show value'}
                    >
                      {valueVisible ? (
                        <EyeOff className="size-4" />
                      ) : (
                        <Eye className="size-4" />
                      )}
                    </Button>
                  </div>
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="text-muted-foreground hover:text-destructive"
                  onClick={() => remove(variable.id)}
                  disabled={disabled}
                  aria-label={`Remove ${variable.key || `variable ${index + 1}`}`}
                >
                  <Trash2 className="size-4" />
                </Button>
              </div>
            )
          })}
        </div>
      )}
    </section>
  )
}
