// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useId } from 'react'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  pluginPermissionLabels,
  pluginPermissionValues,
  type PluginGrantsValues,
} from '@/lib/plugin-grants'

export function PluginGrantFields({
  value,
  onChange,
  disabled,
  requested,
}: {
  value: PluginGrantsValues
  onChange: (value: PluginGrantsValues) => void
  disabled?: boolean
  requested?: PluginGrantsValues['permissions']
}) {
  const id = useId()
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        Choose which Temps services this plugin may use. Nothing is granted by
        default. These controls protect host API access; they do not sandbox
        native plugin code.
      </p>
      <div className="divide-y rounded-lg border">
        {pluginPermissionValues.map((permission) => {
          const text = pluginPermissionLabels[permission]
          const declared = !requested || requested.includes(permission)
          return (
            <div key={permission} className="flex items-start gap-3 p-3">
              <Checkbox
                id={`${id}-${permission}`}
                checked={value.permissions.includes(permission)}
                disabled={
                  disabled ||
                  (!declared && !value.permissions.includes(permission))
                }
                aria-describedby={`${id}-${permission}-description`}
                onCheckedChange={(checked) =>
                  onChange({
                    ...value,
                    permissions:
                      checked === true
                        ? [
                            ...value.permissions.filter(
                              (p) => p !== permission
                            ),
                            permission,
                          ]
                        : value.permissions.filter((p) => p !== permission),
                  })
                }
              />
              <div className="min-w-0 space-y-1">
                <Label htmlFor={`${id}-${permission}`}>{text.label}</Label>
                <p
                  id={`${id}-${permission}-description`}
                  className="text-sm text-muted-foreground"
                >
                  {text.description}
                  {!declared && ' Not requested by this plugin.'}
                </p>
              </div>
            </div>
          )
        })}
      </div>
      {value.permissions.includes('ai_generate') && (
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor={`${id}-daily`}>AI calls per day</Label>
            <Input
              id={`${id}-daily`}
              type="number"
              min={0}
              max={10000}
              step={1}
              value={
                Number.isNaN(value.ai_daily_call_limit)
                  ? ''
                  : value.ai_daily_call_limit
              }
              disabled={disabled}
              onChange={(event) =>
                onChange({
                  ...value,
                  ai_daily_call_limit: event.currentTarget.valueAsNumber,
                })
              }
            />
            <p className="text-xs text-muted-foreground">
              Set to 0 to pause AI calls. Failed attempts may count toward this
              limit.
            </p>
          </div>
          <div className="space-y-2">
            <Label htmlFor={`${id}-tokens`}>
              Maximum output tokens per call
            </Label>
            <Input
              id={`${id}-tokens`}
              type="number"
              min={1}
              max={4096}
              step={1}
              value={
                Number.isNaN(value.ai_max_output_tokens)
                  ? ''
                  : value.ai_max_output_tokens
              }
              disabled={disabled}
              onChange={(event) =>
                onChange({
                  ...value,
                  ai_max_output_tokens: event.currentTarget.valueAsNumber,
                })
              }
            />
            <p className="text-xs text-muted-foreground">
              Call and token limits are not a currency budget; provider charges
              vary.
            </p>
          </div>
        </div>
      )}
    </div>
  )
}
