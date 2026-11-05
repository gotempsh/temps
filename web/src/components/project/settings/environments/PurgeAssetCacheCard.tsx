// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { purgeEnvironmentAssetCache } from '@/api/client'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { SettingsSection } from '@/components/ui/settings-section'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation } from '@tanstack/react-query'
import { HardDrive, RefreshCw } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

export function PurgeAssetCacheCard({
  projectId,
  environmentId,
  environmentName,
}: {
  projectId: number
  environmentId: number
  environmentName: string
}) {
  const [showConfirm, setShowConfirm] = useState(false)

  // A refused purge (missing permission, an unknown environment, a storage
  // failure) used to report "Purged 0 cached assets" as a success, because a
  // raw fetch does not reject on an error status.
  const purgeMutation = useMutation({
    mutationFn: async () =>
      (
        await purgeEnvironmentAssetCache({
          path: { project_id: projectId, environment_id: environmentId },
          throwOnError: true,
        })
      ).data,
    onSuccess: ({ deleted }) => {
      toast.success(`Purged ${deleted} cached asset${deleted !== 1 ? 's' : ''}`)
      setShowConfirm(false)
    },
    onError: (error) => {
      toast.error('Failed to purge asset cache', {
        description: getErrorMessage(
          error,
          'The cached assets were left in place.'
        ),
      })
    },
  })

  return (
    <SettingsSection
      id="setting-environment-asset-cache"
      title="Asset cache"
      icon={HardDrive}
      description="Static assets (JS chunks, CSS, fonts) are cached for stale-chunk fallback. Purge if you need to force-clear cached assets for this environment."
    >
      <Button variant="outline" size="sm" onClick={() => setShowConfirm(true)}>
        <RefreshCw className="h-4 w-4 mr-2" />
        Purge asset cache
      </Button>
      <ConfirmDeleteDialog
        open={showConfirm}
        onOpenChange={setShowConfirm}
        verb="Purge"
        confirmLabel="Purge cache"
        resourceType="asset cache of environment"
        resourceName={environmentName}
        consequences="All cached static assets for this environment are deleted. In-flight users with old HTML may see broken pages until they refresh. Orphaned blobs are cleaned up automatically overnight."
        isPending={purgeMutation.isPending}
        onConfirm={() => purgeMutation.mutate()}
      />
    </SettingsSection>
  )
}
