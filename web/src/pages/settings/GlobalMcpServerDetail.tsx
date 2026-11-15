// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'

import { McpCredentialRevealControls } from '@/components/agents/McpCredentialRevealControls'
import { PageHeader } from '@/components/layout/PageContainer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Loader2,
  Pencil,
  Save,
  Server,
  Trash2,
  X,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import {
  deleteGlobalMcpMutation,
  listGlobalMcpsOptions,
  listGlobalMcpsQueryKey,
  updateGlobalMcpMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { revealGlobalMcpConfig } from '@/api/client/sdk.gen'

export function GlobalMcpServerDetail() {
  const { slug } = useParams<{ slug: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  usePageTitle(`MCP server: ${slug ?? ''}`)

  const {
    data: mcpData,
    isLoading,
    error,
    refetch,
  } = useQuery(listGlobalMcpsOptions())

  const mcp = mcpData?.items.find((m) => m.slug === slug)

  const [isEditing, setIsEditing] = useState(false)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [configText, setConfigText] = useState('')
  const [configError, setConfigError] = useState<string | null>(null)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [loadedMcp, setLoadedMcp] = useState(mcp)

  if (mcp !== loadedMcp) {
    setLoadedMcp(mcp)
    setName(mcp?.name ?? '')
    setDescription(mcp?.description ?? '')
    setConfigText(mcp ? JSON.stringify(mcp.config, null, 2) : '')
    setConfigError(null)
  }

  const handleConfigChange = (value: string) => {
    setConfigText(value)
    try {
      JSON.parse(value)
      setConfigError(null)
    } catch {
      setConfigError('Invalid JSON')
    }
  }

  const updateMutation = useMutation({
    ...updateGlobalMcpMutation(),
    onSuccess: (updatedMcp) => {
      toast.success('MCP server updated')
      queryClient.invalidateQueries({ queryKey: listGlobalMcpsQueryKey() })
      setName(updatedMcp.name)
      setDescription(updatedMcp.description ?? '')
      setConfigText(JSON.stringify(updatedMcp.config, null, 2))
      setConfigError(null)
      setIsEditing(false)
    },
    onError: () => toast.error('Failed to update MCP server'),
  })

  const deleteMutation = useMutation({
    ...deleteGlobalMcpMutation(),
    onSuccess: () => {
      toast.success('MCP server deleted')
      queryClient.invalidateQueries({ queryKey: listGlobalMcpsQueryKey() })
      navigate('/mcp-servers')
    },
    onError: () => {
      setConfirmDelete(false)
      toast.error('Failed to delete MCP server')
    },
  })

  const configSummary = useMemo(() => {
    if (!mcp) return null
    const command = mcp.config.command as string | undefined
    const args = mcp.config.args as string[] | undefined
    const env = mcp.config.env as Record<string, string> | undefined
    return {
      command,
      args,
      envKeys: env ? Object.keys(env) : [],
    }
  }, [mcp])

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error || !mcp) {
    return (
      <div>
        <Link
          to="/mcp-servers"
          className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground mb-4"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to MCP servers
        </Link>
        <Card>
          <CardContent className="py-6">
            <p className="text-sm text-destructive">
              {error ? 'Failed to load MCP server.' : 'MCP server not found.'}{' '}
              <button
                onClick={() => refetch()}
                className="underline hover:no-underline"
              >
                Retry
              </button>
            </p>
          </CardContent>
        </Card>
      </div>
    )
  }

  const handleCancel = () => {
    setName(mcp.name)
    setDescription(mcp.description ?? '')
    setConfigText(JSON.stringify(mcp.config, null, 2))
    setConfigError(null)
    setIsEditing(false)
  }

  const handleSave = (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return
    if (configError) return
    try {
      JSON.parse(configText)
    } catch {
      setConfigError('Invalid JSON — please fix before saving')
      return
    }
    const config = JSON.parse(configText)
    updateMutation.mutate({
      path: { slug: slug! },
      body: {
        name: name.trim(),
        description: description.trim() || undefined,
        config,
      },
    })
  }

  return (
    <div>
      <Link
        to="/mcp-servers"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground mb-4"
      >
        <ArrowLeft className="h-4 w-4" />
        Back to MCP servers
      </Link>

      <div className="mb-6 flex items-start gap-3">
        <div className="mt-1">
          <Server className="h-6 w-6 text-muted-foreground" />
        </div>
        <PageHeader
          className="min-w-0 flex-1"
          title={mcp.name}
          verdict={
            <>
              <Badge variant="secondary" className="font-mono text-xs">
                {mcp.slug}
              </Badge>
              <Badge variant="outline" className="text-xs">
                {mcp.project_id === null
                  ? 'Global'
                  : `Service ${mcp.project_id}`}
              </Badge>
            </>
          }
          description={mcp.description || undefined}
          actions={
            isEditing ? (
              <>
                <Button
                  variant="outline"
                  onClick={handleCancel}
                  disabled={updateMutation.isPending}
                >
                  <X className="h-4 w-4 mr-2" />
                  Cancel
                </Button>
                <Button
                  onClick={handleSave}
                  disabled={
                    updateMutation.isPending ||
                    !name.trim() ||
                    configError !== null
                  }
                >
                  {updateMutation.isPending ? (
                    <Loader2 className="h-4 w-4 animate-spin mr-2" />
                  ) : (
                    <Save className="h-4 w-4 mr-2" />
                  )}
                  Save
                </Button>
              </>
            ) : (
              <>
                <Button variant="outline" onClick={() => setIsEditing(true)}>
                  <Pencil className="h-4 w-4 mr-2" />
                  Edit
                </Button>
                <Button
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                  onClick={() => setConfirmDelete(true)}
                >
                  <Trash2 className="h-4 w-4 mr-2" />
                  Delete
                </Button>
              </>
            )
          }
        />
      </div>

      <div>
        <Card>
          <CardHeader>
            <CardTitle className="text-base">
              {isEditing ? 'Edit MCP server' : 'Configuration'}
            </CardTitle>
          </CardHeader>
          <CardContent>
            {isEditing ? (
              <form onSubmit={handleSave} className="space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="mcp-name">Name</Label>
                  <Input
                    id="mcp-name"
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    required
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="mcp-description">Description</Label>
                  <Input
                    id="mcp-description"
                    value={description}
                    onChange={(e) => setDescription(e.target.value)}
                    placeholder="Brief description of what this MCP server provides"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="mcp-config">Configuration (JSON)</Label>
                  <p className="text-xs text-muted-foreground">
                    Follows the{' '}
                    <code className="bg-muted px-1 rounded">mcpServers</code>{' '}
                    format: command, args, and optional env vars.
                  </p>
                  <Textarea
                    id="mcp-config"
                    value={configText}
                    onChange={(e) => handleConfigChange(e.target.value)}
                    required
                    className="font-mono text-sm min-h-[280px]"
                    rows={16}
                  />
                  {configError && (
                    <p className="text-xs text-destructive">{configError}</p>
                  )}
                </div>
                <McpCredentialRevealControls
                  key={`${mcp.slug}:${mcp.updated_at}:${isEditing}`}
                  configText={configText}
                  onConfigTextChange={handleConfigChange}
                  onRevealCredential={async (field) => {
                    const { data } = await revealGlobalMcpConfig({
                      path: { slug: mcp.slug, field },
                      throwOnError: true,
                    })
                    return data.value
                  }}
                />
              </form>
            ) : (
              <div className="space-y-3">
                {configSummary?.command && (
                  <div>
                    <div className="text-xs font-medium text-muted-foreground mb-1">
                      Command
                    </div>
                    <code className="text-xs bg-muted px-1.5 py-0.5 rounded font-mono break-all">
                      {configSummary.command}
                      {configSummary.args
                        ? ` ${configSummary.args.join(' ')}`
                        : ''}
                    </code>
                  </div>
                )}
                {configSummary && configSummary.envKeys.length > 0 && (
                  <div>
                    <div className="text-xs font-medium text-muted-foreground mb-1">
                      Environment variables
                    </div>
                    <div className="flex flex-wrap gap-1">
                      {configSummary.envKeys.map((k) => (
                        <Badge
                          key={k}
                          variant="outline"
                          className="font-mono text-xs"
                        >
                          {k}
                        </Badge>
                      ))}
                    </div>
                  </div>
                )}
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Raw config
                  </div>
                  <div className="rounded-md border bg-muted/50 p-3">
                    <pre
                      tabIndex={0}
                      className="text-xs whitespace-pre-wrap font-mono overflow-x-auto"
                    >
                      <HighlightedCode
                        code={JSON.stringify(mcp.config, null, 2)}
                        language="json"
                      />
                    </pre>
                  </div>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      <ConfirmDeleteDialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        resourceType="global MCP server"
        resourceName={mcp?.name ?? slug ?? ''}
        consequences="All services that reference it will lose access."
        isPending={deleteMutation.isPending}
        onConfirm={() => deleteMutation.mutate({ path: { slug: slug! } })}
      />
    </div>
  )
}
