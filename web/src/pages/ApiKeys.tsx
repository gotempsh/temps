// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useNavigate } from 'react-router'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { toast } from 'sonner'
import {
  listApiKeys,
  deleteApiKey,
  activateApiKey,
  deactivateApiKey,
  type ApiKeyResponse,
} from '@/api/client'
import { ApiKeyTable, ApiKeyDeleteModal } from '@/components/api-keys'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function ApiKeys() {
  usePageTitle('API Keys')
  const navigate = useNavigate()
  const [deleteModalOpen, setDeleteModalOpen] = useState(false)
  const [selectedKey, setSelectedKey] = useState<ApiKeyResponse | null>(null)
  const queryClient = useQueryClient()

  // Fetch API keys
  const { data: apiKeysData, isLoading } = useQuery({
    queryKey: ['apiKeys'],
    queryFn: async () => {
      const response = await listApiKeys({
        query: { page: 1, page_size: 100 },
      })
      return response.data
    },
  })

  const apiKeys = apiKeysData?.api_keys

  // Delete mutation
  const deleteMutation = useMutation({
    mutationFn: (id: number) => deleteApiKey({ path: { id } }),
    meta: {
      errorTitle: 'Failed to delete API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
      setDeleteModalOpen(false)
      setSelectedKey(null)
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
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
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
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
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
    setSelectedKey(key)
    setDeleteModalOpen(true)
  }

  const handleDeleteConfirm = (id: number) => {
    deleteMutation.mutate(id)
  }

  const handleCreateClick = () => {
    navigate('/settings/keys/new')
  }

  return (
    <div className="container mx-auto px-4 py-6 space-y-6 sm:px-6">
      {/* Page Header */}
      <div className="flex flex-col gap-3 sm:flex-row sm:justify-between sm:items-center">
        <div>
          <h1 className="text-2xl sm:text-3xl font-bold">API Keys</h1>
          <p className="text-muted-foreground mt-2">
            Manage your API keys for programmatic access
          </p>
        </div>
        <CreateActionButton onClick={handleCreateClick} label="Create API Key" />
      </div>

      {/* Statistics Cards */}
      {apiKeys && apiKeys.length > 0 && (
        <div className="grid gap-3 grid-cols-1 sm:grid-cols-3 sm:gap-4">
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-medium">Total Keys</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{apiKeys.length}</div>
              <p className="text-xs text-muted-foreground">API keys created</p>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-medium">Active Keys</CardTitle>
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
                Expiring Soon
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
            onView={handleView}
            onEdit={handleEdit}
            onDelete={handleDelete}
            onActivate={(id) => activateMutation.mutate(id)}
            onDeactivate={(id) => deactivateMutation.mutate(id)}
            onCreateClick={handleCreateClick}
          />
        </CardContent>
      </Card>

      {/* Delete Modal */}
      <ApiKeyDeleteModal
        open={deleteModalOpen}
        onOpenChange={(open) => {
          setDeleteModalOpen(open)
          if (!open) setSelectedKey(null)
        }}
        apiKey={selectedKey}
        onConfirm={handleDeleteConfirm}
        isPending={deleteMutation.isPending}
      />
    </div>
  )
}
