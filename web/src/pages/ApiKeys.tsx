// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@/components/layout/PageContainer'

import { useState } from 'react'
import { useNavigate } from 'react-router'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { toast } from 'sonner'
import {
  deleteApiKey,
  activateApiKey,
  deactivateApiKey,
  type ApiKeyResponse,
} from '@/api/client'
import {
  listApiKeysOptions,
  listApiKeysQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { ApiKeyTable } from '@/components/api-keys'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function ApiKeys() {
  usePageTitle('API keys')
  const navigate = useNavigate()
  const [keyToDelete, setKeyToDelete] = useState<ApiKeyResponse | null>(null)
  const queryClient = useQueryClient()

  // The generated options throw the server's Problem Details on a non-2xx
  // response, so a refused or failed read reaches react-query as an error
  // instead of resolving to `undefined` and looking like an empty list.
  const apiKeysQuery = useQuery({
    ...listApiKeysOptions({ query: { page: 1, page_size: 100 } }),
  })
  const { data: apiKeysData, isLoading } = apiKeysQuery

  const apiKeys = apiKeysData?.api_keys

  // Delete mutation
  const deleteMutation = useMutation({
    mutationFn: (id: number) => deleteApiKey({ path: { id } }),
    meta: {
      errorTitle: 'Failed to delete API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: listApiKeysQueryKey() })
      setKeyToDelete(null)
      toast.success('API key deleted successfully')
    },
  })

  // Activate mutation
  const activateMutation = useMutation({
    mutationFn: (id: number) => activateApiKey({ path: { id } }),
    meta: {
      errorTitle: 'Failed to activate API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: listApiKeysQueryKey() })
      toast.success('API key activated')
    },
  })

  // Deactivate mutation
  const deactivateMutation = useMutation({
    mutationFn: (id: number) => deactivateApiKey({ path: { id } }),
    meta: {
      errorTitle: 'Failed to deactivate API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: listApiKeysQueryKey() })
      toast.success('API key deactivated')
    },
  })

  // Handlers
  const handleView = (key: ApiKeyResponse) => {
    navigate(`/settings/keys/${key.id}`)
  }

  const handleEdit = (key: ApiKeyResponse) => {
    navigate(`/settings/keys/${key.id}/edit`)
  }

  const handleDelete = (key: ApiKeyResponse) => {
    setKeyToDelete(key)
  }

  const handleCreateClick = () => {
    navigate('/settings/keys/new')
  }

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="API keys"
        description="Manage your API keys for programmatic access"
        actions={
          <CreateActionButton
            onClick={handleCreateClick}
            label="Create API key"
          />
        }
      />

      {/* Statistics Cards */}
      {apiKeys && apiKeys.length > 0 && (
        <div className="grid gap-3 grid-cols-1 sm:grid-cols-3 sm:gap-4">
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-medium">Total keys</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{apiKeys.length}</div>
              <p className="text-xs text-muted-foreground">API keys created</p>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-medium">Active keys</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">
                {apiKeys.filter((k: ApiKeyResponse) => k.is_active).length}
              </div>
              <p className="text-xs text-muted-foreground">Currently active</p>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-medium">
                Expiring soon
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">
                {
                  apiKeys.filter((k: ApiKeyResponse) => {
                    if (!k.expires_at) return false
                    const daysUntilExpiry = Math.ceil(
                      (new Date(k.expires_at).getTime() -
                        new Date().getTime()) /
                        (1000 * 60 * 60 * 24)
                    )
                    return daysUntilExpiry > 0 && daysUntilExpiry <= 30
                  }).length
                }
              </div>
              <p className="text-xs text-muted-foreground">Within 30 days</p>
            </CardContent>
          </Card>
        </div>
      )}

      {/* API Keys Table */}
      <Card>
        <CardContent className="pt-6">
          <ApiKeyTable
            apiKeys={apiKeys}
            isLoading={isLoading}
            error={apiKeysQuery.isError ? apiKeysQuery.error : undefined}
            onRetry={() => apiKeysQuery.refetch()}
            retrying={apiKeysQuery.isFetching}
            onView={handleView}
            onEdit={handleEdit}
            onDelete={handleDelete}
            onActivate={(id) => activateMutation.mutate(id)}
            onDeactivate={(id) => deactivateMutation.mutate(id)}
            onCreateClick={handleCreateClick}
          />
        </CardContent>
      </Card>

      <ConfirmDeleteDialog
        open={keyToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setKeyToDelete(null)
        }}
        resourceType="API key"
        resourceName={keyToDelete?.name ?? ''}
        consequences={
          keyToDelete
            ? `Key ${keyToDelete.key_prefix}... (${keyToDelete.role_type}) stops working immediately; any application using it loses access.`
            : undefined
        }
        isPending={deleteMutation.isPending}
        onConfirm={() => {
          if (keyToDelete) deleteMutation.mutate(keyToDelete.id)
        }}
      />
    </div>
  )
}
