// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Badge } from '@/components/ui/badge'
import {
  Sparkles,
  Folder,
  Layers,
  Rocket,
  Radio,
  Eye,
  Pencil,
  ShieldCheck,
} from 'lucide-react'
import { type PluginPermissionRequirement } from '@/lib/plugin-grants'
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
  requirements,
}: {
  value: PluginGrantsValues
  onChange: (value: PluginGrantsValues) => void
  disabled?: boolean
  requested?: PluginGrantsValues['permissions']
  requirements?: PluginPermissionRequirement[] | null
}) {
  const id = useId()
  const icons = {
    ai_generate: Sparkles,
    projects_read: Folder,
    environments_read: Layers,
    deployments_read: Rocket,
    events_read: Radio,
    api_read: Eye,
    api_write: Pencil,
  }
  const visible = requirements
    ? pluginPermissionValues.filter((p) =>
        requirements.some((r) => r.permission === p)
      )
    : pluginPermissionValues
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        {requirements
          ? 'Approve the access you want to grant. Optional permissions enable extra features.'
          : 'Permission requirements are not published. Review the source before choosing access.'}{' '}
        Nothing is granted by default.
      </p>
      {requirements?.length === 0 && (
        <p className="flex items-center gap-2 text-sm">
          <ShieldCheck className="size-4 text-muted-foreground" />
          No host API permissions requested.
        </p>
      )}
      <div className="divide-y rounded-lg border">
        {visible.map((permission) => {
          const requirement = requirements?.find(
            (r) => r.permission === permission
          )
          const Icon = icons[permission]
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
                <div className="flex flex-wrap items-center gap-2">
                  <Icon
                    className="size-4 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <Label htmlFor={`${id}-${permission}`}>{text.label}</Label>
                  {requirement && (
                    <Badge variant="secondary">
                      {requirement.required ? 'Required' : 'Optional'}
                    </Badge>
                  )}
                </div>
                <p
                  id={`${id}-${permission}-description`}
                  className="text-sm text-muted-foreground"
                >
                  {requirement?.reason || text.description}
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
