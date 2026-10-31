// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Plus, Trash2 } from 'lucide-react'
import type { RateLimitSettings } from '@/api/platformSettings'
import { DEFAULT_RATE_LIMITING } from './rate-limiting'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'

/** A cleared number input reads as NaN; show it empty rather than "NaN". */
function numberValue(value: number | undefined): number | '' {
  return value === undefined || Number.isNaN(value) ? '' : value
}

/**
 * The per-IP request limits. The Security page and the Rate limiting page
 * both edit `rate_limiting` through this one group, so the two never drift.
 * The caller owns the form: `onChange` receives the whole new value.
 */
export function RateLimitFields({
  value,
  onChange,
}: {
  value: RateLimitSettings | undefined
  onChange: (next: RateLimitSettings) => void
}) {
  const current = value ?? DEFAULT_RATE_LIMITING
  const whitelist = current.whitelist_ips ?? []
  const update = (patch: Partial<RateLimitSettings>) =>
    onChange({ ...current, ...patch })

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-4">
        <div className="space-y-0.5">
          <Label htmlFor="rate-limiting-enabled">Enable rate limiting</Label>
          <p className="text-sm text-muted-foreground">
            Limit requests per IP address
          </p>
        </div>
        <Switch
          id="rate-limiting-enabled"
          checked={current.enabled}
          onCheckedChange={(enabled) => update({ enabled })}
        />
      </div>

      {current.enabled && (
        <>
          <Separator />
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="max-requests-per-minute">
                Max requests per minute
              </Label>
              <Input
                id="max-requests-per-minute"
                type="number"
                min="1"
                placeholder="60"
                value={numberValue(current.max_requests_per_minute)}
                onChange={(e) =>
                  update({ max_requests_per_minute: e.target.valueAsNumber })
                }
              />
              <p className="text-sm text-muted-foreground">
                Maximum requests allowed per IP per minute
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="max-requests-per-hour">
                Max requests per hour
              </Label>
              <Input
                id="max-requests-per-hour"
                type="number"
                min="1"
                placeholder="1000"
                value={numberValue(current.max_requests_per_hour)}
                onChange={(e) =>
                  update({ max_requests_per_hour: e.target.valueAsNumber })
                }
              />
              <p className="text-sm text-muted-foreground">
                Maximum requests allowed per IP per hour
              </p>
            </div>
          </div>

          <Separator />

          <div>
            <Label>Whitelist IPs</Label>
            <p className="mb-2 text-sm text-muted-foreground">
              IPs that bypass rate limiting
            </p>
            <div className="space-y-2">
              {whitelist.map((ip, index) => (
                <div key={`whitelist-${index}`} className="flex gap-2">
                  <Input
                    aria-label={`Whitelisted IP ${index + 1}`}
                    value={ip}
                    onChange={(e) =>
                      update({
                        whitelist_ips: whitelist.map((entry, i) =>
                          i === index ? e.target.value : entry
                        ),
                      })
                    }
                    placeholder="192.168.1.1 or 10.0.0.0/24"
                  />
                  <Button
                    aria-label="Remove IP address"
                    type="button"
                    variant="outline"
                    size="icon"
                    onClick={() =>
                      update({
                        whitelist_ips: whitelist.filter((_, i) => i !== index),
                      })
                    }
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              ))}
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => update({ whitelist_ips: [...whitelist, ''] })}
              >
                <Plus className="h-4 w-4 mr-2" />
                Add whitelist IP
              </Button>
            </div>
          </div>
        </>
      )}
    </div>
  )
}
