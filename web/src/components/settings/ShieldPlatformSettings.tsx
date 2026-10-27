// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getShieldSettingsOptions,
  getShieldSettingsQueryKey,
  updateShieldSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ShieldEntriesPanel } from '@/components/shield/ShieldEntriesPanel'
import { ShieldProposalsPanel } from '@/components/shield/ShieldProposalsPanel'
import { Label } from '@/components/ui/label'
import { ReadFailure } from '@/components/ui/read-failure'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

/**
 * The platform's part of Shield (ADR 066): whether what one service's
 * visitors did counts for every service (D9), and the entries that apply to
 * every service. Each service picks its own mode in its Security settings.
 */
export function ShieldPlatformSettings() {
  const queryClient = useQueryClient()
  const settings = useQuery({ ...getShieldSettingsOptions() })
  const update = useMutation({
    ...updateShieldSettingsMutation(),
    meta: { errorTitle: 'Failed to update Shield settings' },
    onSuccess: (saved) => {
      queryClient.setQueryData(getShieldSettingsQueryKey(), saved)
      toast.success(
        saved.share_observations
          ? 'Shield now proposes blocks from attacks across all services'
          : 'Shield no longer learns from attacks'
      )
    },
  })
  // Default on, as the server's default is.
  const sharing = settings.data?.share_observations ?? true

  return (
    <div className="space-y-6">
      <p className="text-sm text-muted-foreground">
        Shield checks every request against public blocklists, known exploit
        probes and the entries below. Each service chooses what Shield does with
        those verdicts, from only recording them to blocking and challenging, in
        its own Security settings.
      </p>

      <div className="space-y-2">
        {settings.error ? (
          <ReadFailure
            resource="Shield settings"
            error={settings.error}
            cached={settings.data !== undefined}
            onRetry={() => settings.refetch()}
            retrying={settings.isFetching}
            embedded
          />
        ) : settings.data === undefined ? (
          <Skeleton
            role="status"
            aria-label="Loading Shield settings"
            className="h-12 w-full"
          />
        ) : (
          <div className="flex items-start justify-between gap-4">
            <div className="space-y-0.5">
              <Label htmlFor="shield-share-observations">
                Learn from attacks across all services
              </Label>
              <p className="text-sm text-muted-foreground">
                Shield proposes blocking an address that sends exploit probes to
                one service, for 7 days, and challenging one that fails or
                abandons three challenges within an hour, for 24 hours. A
                proposal applies on every service only once you approve it. Off,
                Shield proposes nothing and acts only on the lists and your
                entries.
              </p>
            </div>
            <Switch
              id="shield-share-observations"
              checked={sharing}
              disabled={update.isPending}
              onCheckedChange={(checked) =>
                update.mutate({ body: { share_observations: checked } })
              }
            />
          </div>
        )}
      </div>

      <Separator />

      <div className="space-y-3">
        <h3 className="text-sm font-medium">Proposals</h3>
        <ShieldProposalsPanel learning={sharing} />
      </div>

      <Separator />

      <div className="space-y-3">
        <h3 className="text-sm font-medium">Platform entries</h3>
        <ShieldEntriesPanel scope={{ kind: 'platform' }} />
      </div>
    </div>
  )
}
