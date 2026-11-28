// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useId } from 'react'
import { Link } from 'react-router'
import { Globe, Radio, Unlock } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import {
  MAX_WRITE_SERVICES,
  pluginOutboundLabel,
  pluginWriteAreaLabels,
  pluginWriteAreaValues,
  type PluginGrantsValues,
} from '@/lib/plugin-grants'

/** A service a plugin's changes can be limited to. */
export type WriteScopeService = { id: number; name: string }

/**
 * How far a permission to change reaches: which areas of Temps, and in
 * which services. Shown only once the permission itself is allowed.
 */
export function PluginWriteScopeFields({
  value,
  onChange,
  disabled,
  services,
  servicesLoading,
  servicesFailed,
  servicesTotal,
}: {
  value: PluginGrantsValues
  onChange: (value: PluginGrantsValues) => void
  disabled?: boolean
  services: WriteScopeService[]
  servicesLoading?: boolean
  servicesFailed?: boolean
  servicesTotal?: number
}) {
  const id = useId()
  const limited = value.write_project_ids !== null
  const chosen = value.write_project_ids ?? []
  // A chosen service that is not on the first page is still listed, so it
  // can be seen and removed.
  const listed = [
    ...services,
    ...chosen
      .filter((service) => !services.some((entry) => entry.id === service))
      .map((service) => ({ id: service, name: `Service #${service}` })),
  ]
  return (
    <div className="space-y-4 rounded-lg border p-3">
      <fieldset className="space-y-2" disabled={disabled}>
        <legend className="text-sm font-medium">What it may change</legend>
        {value.write_areas.length === 0 && (
          <p className="text-sm text-muted-foreground">
            Nothing yet. Until an area is chosen, every change this plugin
            attempts is refused.
          </p>
        )}
        {pluginWriteAreaValues.map((area) => {
          const text = pluginWriteAreaLabels[area]
          const checked = value.write_areas.includes(area)
          return (
            <div key={area} className="flex items-start gap-3">
              <Checkbox
                id={`${id}-area-${area}`}
                checked={checked}
                disabled={disabled}
                aria-describedby={`${id}-area-${area}-description`}
                onCheckedChange={(next) =>
                  onChange({
                    ...value,
                    write_areas:
                      next === true
                        ? pluginWriteAreaValues.filter(
                            (entry) =>
                              entry === area ||
                              value.write_areas.includes(entry)
                          )
                        : value.write_areas.filter((entry) => entry !== area),
                  })
                }
              />
              <div className="min-w-0 space-y-0.5">
                <Label htmlFor={`${id}-area-${area}`}>{text.label}</Label>
                <p
                  id={`${id}-area-${area}-description`}
                  className="text-sm text-muted-foreground"
                >
                  {text.description}
                </p>
              </div>
            </div>
          )
        })}
      </fieldset>
      <fieldset className="space-y-2" disabled={disabled}>
        <legend className="text-sm font-medium">Where</legend>
        <RadioGroup
          value={limited ? 'some' : 'all'}
          disabled={disabled}
          onValueChange={(next) =>
            onChange({
              ...value,
              write_project_ids: next === 'some' ? chosen : null,
            })
          }
        >
          <div className="flex items-center gap-3">
            <RadioGroupItem id={`${id}-where-all`} value="all" />
            <Label htmlFor={`${id}-where-all`}>Every service</Label>
          </div>
          <div className="flex items-center gap-3">
            <RadioGroupItem id={`${id}-where-some`} value="some" />
            <Label htmlFor={`${id}-where-some`}>Only chosen services</Label>
          </div>
        </RadioGroup>
        {limited && (
          <div className="space-y-2 pl-7">
            <p className="text-sm text-muted-foreground">
              A plugin limited to some services cannot create new services.
            </p>
            {servicesLoading ? (
              <div
                role="status"
                aria-label="Loading services"
                className="space-y-2"
              >
                {[0, 1, 2].map((row) => (
                  <Skeleton key={row} className="h-5 w-48" />
                ))}
              </div>
            ) : servicesFailed ? (
              <p role="alert" className="text-sm text-destructive">
                Services could not be listed, so none can be chosen. Close this
                dialog and try again.
              </p>
            ) : listed.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                There are no services yet.
              </p>
            ) : (
              <div className="max-h-48 space-y-2 overflow-y-auto">
                {listed.map((service) => (
                  <div key={service.id} className="flex items-center gap-3">
                    <Checkbox
                      id={`${id}-service-${service.id}`}
                      checked={chosen.includes(service.id)}
                      disabled={disabled}
                      onCheckedChange={(next) =>
                        onChange({
                          ...value,
                          write_project_ids:
                            next === true
                              ? [
                                  ...chosen.filter(
                                    (entry) => entry !== service.id
                                  ),
                                  service.id,
                                ]
                              : chosen.filter((entry) => entry !== service.id),
                        })
                      }
                    />
                    <Label
                      htmlFor={`${id}-service-${service.id}`}
                      className="min-w-0 [overflow-wrap:anywhere]"
                    >
                      {service.name}
                    </Label>
                  </div>
                ))}
              </div>
            )}
            {(servicesTotal ?? 0) > services.length && (
              <p className="text-xs text-muted-foreground">
                Showing the first {services.length} of {servicesTotal} services.
                At most {MAX_WRITE_SERVICES} can be chosen.
              </p>
            )}
          </div>
        )}
      </fieldset>
    </div>
  )
}

/** What the permission screen is told about a plugin beyond its grants. */
export type PluginReachInfo = {
  public_paths: string[]
  events: string[]
  outbound_network: { profile: string; detail: string; settings_path: string }
}

/**
 * What a plugin can reach without asking: the routes it serves to anyone,
 * the events it listens for, and what its container may connect to. None of
 * it is chosen here, so it is stated, with where to change it.
 */
export function PluginReach({ reach }: { reach: PluginReachInfo }) {
  const outbound = pluginOutboundLabel(reach.outbound_network.profile)
  return (
    <dl className="divide-y rounded-lg border text-sm">
      <div className="space-y-1 p-3">
        <dt className="flex items-center gap-2 font-medium">
          <Unlock className="size-4 text-muted-foreground" aria-hidden="true" />
          Routes open without signing in
        </dt>
        <dd className="text-muted-foreground">
          {reach.public_paths.length === 0 ? (
            'None. Every route of this plugin needs a Temps session.'
          ) : (
            <>
              Anyone who can reach this server can call these; the plugin checks
              callers itself.
              <span className="mt-1 flex flex-wrap gap-1">
                {reach.public_paths.map((path) => (
                  <code
                    key={path}
                    className="rounded bg-muted px-1.5 py-0.5 text-xs text-foreground [overflow-wrap:anywhere]"
                  >
                    {path}
                  </code>
                ))}
              </span>
            </>
          )}
        </dd>
      </div>
      <div className="space-y-1 p-3">
        <dt className="flex items-center gap-2 font-medium">
          <Radio className="size-4 text-muted-foreground" aria-hidden="true" />
          Events it listens for
        </dt>
        <dd className="text-muted-foreground">
          {reach.events.length === 0 ? (
            'None.'
          ) : (
            <>
              Delivered only while “Receive platform events” is allowed.
              <span className="mt-1 flex flex-wrap gap-1">
                {reach.events.map((event) => (
                  <code
                    key={event}
                    className="rounded bg-muted px-1.5 py-0.5 text-xs text-foreground [overflow-wrap:anywhere]"
                  >
                    {event}
                  </code>
                ))}
              </span>
            </>
          )}
        </dd>
      </div>
      <div className="space-y-1 p-3">
        <dt className="flex flex-wrap items-center gap-2 font-medium">
          <Globe className="size-4 text-muted-foreground" aria-hidden="true" />
          Outbound network
          <Badge variant={outbound.tone === 'open' ? 'outline' : 'secondary'}>
            {outbound.label}
          </Badge>
        </dt>
        <dd className="text-muted-foreground">
          {reach.outbound_network.detail}{' '}
          <Link className="underline" to={reach.outbound_network.settings_path}>
            Network policy
          </Link>
        </dd>
      </div>
    </dl>
  )
}
