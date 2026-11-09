// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
import { Callout, PageContainer } from '@temps-sdk/ds'
import { type AccessBundleResponse } from '@/api/client'
import {
  accessCapabilityOptions,
  getAccessVaultOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { accessBundleOptions } from '@/components/service-access/access-bundle-query'
import { AccessBundleEditor } from '@/components/service-access/AccessBundleEditor'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

export default function AccessBundlePage() {
  const { projectId = '', vaultId = '', bundleId } = useParams()
  return (
    <BundlePage
      key={`${projectId}:${vaultId}:${bundleId ?? 'new'}`}
      projectId={Number(projectId)}
      vaultId={vaultId}
      bundleId={bundleId}
    />
  )
}
function BundlePage({
  projectId,
  vaultId,
  bundleId,
}: {
  projectId: number
  vaultId: string
  bundleId?: string
}) {
  const navigate = useNavigate()
  const backPath = `/vault/projects/${projectId}/vaults/${encodeURIComponent(vaultId)}?tab=bundles`
  const valid =
    Number.isInteger(projectId) &&
    projectId > 0 &&
    projectId <= 2147483647 &&
    !!vaultId
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId } }),
    enabled: valid,
    retry: false,
  })
  const vault = useQuery({
    ...getAccessVaultOptions({
      path: { project_id: projectId, vault_id: vaultId },
    }),
    enabled: valid,
    retry: false,
    refetchOnMount: 'always',
  })
  const bundle = useQuery({
    ...accessBundleOptions(projectId, vaultId, bundleId ?? ''),
    enabled: valid && !!bundleId,
  })
  const [snapshot, setSnapshot] = useState<{ bundle?: AccessBundleResponse }>()
  if (
    !snapshot &&
    vault.isFetchedAfterMount &&
    vault.isSuccess &&
    (!bundleId || (bundle.isFetchedAfterMount && bundle.isSuccess))
  )
    setSnapshot({ bundle: bundle.data })
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vault', href: '/vault' },
      { label: vault.data?.vault.name ?? 'Vault', href: backPath },
      { label: bundleId ? 'Edit access bundle' : 'Create access bundle' },
    ])
    return () => setBreadcrumbs([])
  }, [setBreadcrumbs, backPath, vault.data?.vault.name, bundleId])
  const error = vault.error || (bundleId && bundle.error) || capability.error
  const canWrite =
    capability.data?.can_manage_services === true && !capability.isError
  const retry = () => {
    void vault.refetch()
    void capability.refetch()
    if (bundleId) void bundle.refetch()
  }
  return (
    <PageContainer width="form">
      {!valid ? (
        <Callout tone="error" title="Invalid vault link">
          <Button onClick={() => navigate('/vault')}>Back to Vault</Button>
        </Callout>
      ) : (
        <>
          {error && (
            <Callout tone="error" title="Could not load access bundle">
              {problemDetail(
                error,
                'Your draft is preserved. Retry before saving.'
              )}
              <Button variant="outline" onClick={retry}>
                Retry
              </Button>
            </Callout>
          )}
          {!snapshot || capability.isPending ? (
            !error && (
              <Skeleton className="h-64" aria-label="Loading access bundle" />
            )
          ) : (
            <>
              {!canWrite && !capability.isError && (
                <Callout title="Access bundles are read-only">
                  Project write permission is required to save a bundle.
                </Callout>
              )}
              <fieldset disabled={!canWrite || !!error} className="min-w-0">
                <AccessBundleEditor
                  projectId={projectId}
                  vaultId={vaultId}
                  initialBundle={snapshot.bundle}
                  services={vault.data?.services ?? []}
                  canWrite={canWrite && !error}
                  onClose={() => navigate(backPath)}
                />
              </fieldset>
            </>
          )}
          {(!snapshot || !canWrite || !!error) && (
            <Button variant="outline" onClick={() => navigate(backPath)}>
              Back to access bundles
            </Button>
          )}
        </>
      )}
    </PageContainer>
  )
}
