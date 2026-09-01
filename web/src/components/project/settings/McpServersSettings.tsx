// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { McpCredentialRevealControls } from '@/components/agents/McpCredentialRevealControls'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { EllipsisVertical, Loader2, Plus, Server } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  deleteMcpMutation,
  listMcpsOptions,
  listMcpsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { createMcp, revealMcpConfig, updateMcp } from '@/api/client/sdk.gen'
import type { McpDefinitionResponse as McpDefinition } from '@/api/client/types.gen'

interface McpServersSettingsProps {
  project: ProjectResponse
}

export function McpServersSettings({ project }: McpServersSettingsProps) {
  const queryClient = useQueryClient()
  const [mcpToDelete, setMcpToDelete] = useState<string | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingMcp, setEditingMcp] = useState<McpDefinition | null>(null)

  const mcpListKey = listMcpsQueryKey({ path: { project_id: project.id } })

  const {
    data: mcpServersData,
    isLoading,
    error,
    refetch,
  } = useQuery(listMcpsOptions({ path: { project_id: project.id } }))

  const mcpServers = mcpServersData?.items ?? []

  const deleteMutation = useMutation({
    ...deleteMcpMutation(),
    onSuccess: () => {
      toast.success('MCP server deleted')
      queryClient.invalidateQueries({ queryKey: mcpListKey })
      setMcpToDelete(null)
    },
    onError: () => toast.error('Failed to delete MCP server'),
  })

  const openCreate = () => {
    setEditingMcp(null)
    setDialogOpen(true)
  }

  const openEdit = (mcp: McpDefinition) => {
    setEditingMcp(mcp)
    setDialogOpen(true)
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-lg font-semibold">MCP Servers</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Define MCP server configurations that can be assigned to AI
            workflows. Configs are merged into{' '}
            <code className="text-xs bg-muted px-1 rounded">
              .claude/settings.json
            </code>{' '}
            at runtime.
          </p>
        </div>
        <CreateActionButton
          onClick={openCreate}
          disabled={isLoading}
          label="Add MCP Server"
        />
      </div>

      {error && (
        <Card>
          <CardContent className="py-6">
            <p className="text-sm text-destructive">
              Failed to load MCP servers.{' '}
              <button
                onClick={() => refetch()}
                className="underline hover:no-underline"
              >
                Retry
              </button>
            </p>
          </CardContent>
        </Card>
      )}

      {isLoading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </div>
      ) : !error && mcpServers.length > 0 ? (
        <div className="space-y-3">
          {mcpServers.map((mcp) => {
            const command = mcp.config.command as string | undefined
            const args = mcp.config.args as string[] | undefined
            return (
              <Card key={mcp.id}>
                <CardHeader className="pb-2">
                  <div className="flex items-start justify-between">
                    <div className="flex items-start gap-3 flex-1">
                      <div className="mt-1">
                        <Server className="h-5 w-5 text-muted-foreground" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2 mb-1">
                          <CardTitle className="text-base">
                            {mcp.name}
                          </CardTitle>
                          <Badge
                            variant="secondary"
                            className="font-mono text-xs"
                          >
                            {mcp.slug}
                          </Badge>
                        </div>
                        {mcp.description && (
                          <CardDescription className="text-xs">
                            {mcp.description}
                          </CardDescription>
                        )}
                      </div>
                    </div>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="ghost" size="icon" className="h-8 w-8">
                          <EllipsisVertical className="h-4 w-4" />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem onClick={() => openEdit(mcp)}>
                          Edit
                        </DropdownMenuItem>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem
                          className="text-destructive"
                          onClick={() => setMcpToDelete(mcp.slug)}
                        >
                          Delete
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                </CardHeader>
                <CardContent>
                  {command && (
                    <div className="text-xs text-muted-foreground mb-2">
                      <span className="font-medium">Command:</span>{' '}
                      <code className="bg-muted px-1 rounded">
                        {command}
                        {args ? ` ${args.join(' ')}` : ''}
                      </code>
                    </div>
                  )}
                  <div className="rounded-md border bg-muted/50 p-3">
                    <pre className="text-xs text-muted-foreground whitespace-pre-wrap line-clamp-4 font-mono">
                      {JSON.stringify(mcp.config, null, 2)}
                    </pre>
                  </div>
                </CardContent>
              </Card>
            )
          })}
        </div>
      ) : !error ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Server className="h-12 w-12 text-muted-foreground/50 mb-4" />
            <h3 className="text-lg font-semibold mb-2">
              No MCP servers defined
            </h3>
            <p className="text-sm text-muted-foreground text-center mb-4 max-w-md">
              MCP servers extend AI workflow capabilities with tools like
              browser automation, file system access, database queries, and
              more. Define server configs here and assign them to workflows.
            </p>
            <Button onClick={openCreate}>
              <Plus className="h-4 w-4 mr-2" />
              Create Your First MCP Server
            </Button>
          </CardContent>
        </Card>
      ) : null}

      <McpDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        projectId={project.id}
        mcp={editingMcp}
        onSuccess={() => {
          queryClient.invalidateQueries({ queryKey: mcpListKey })
          setDialogOpen(false)
        }}
      />

      <AlertDialog
        open={mcpToDelete !== null}
        onOpenChange={() => setMcpToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete MCP server?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete this MCP server definition. Workflows
              referencing it will no longer have access to it.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (mcpToDelete)
                  deleteMutation.mutate({
                    path: { project_id: project.id, slug: mcpToDelete },
                  })
              }}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

// ── Create / Edit Dialog ──

const MCP_CONFIG_EXAMPLE = `{
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"],
  "env": {}
}`

interface McpDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  mcp: McpDefinition | null
  onSuccess: () => void
}

function McpDialog({
  open,
  onOpenChange,
  projectId,
  mcp,
  onSuccess,
}: McpDialogProps) {
  const isEdit = !!mcp
  const [slug, setSlug] = useState('')
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [configText, setConfigText] = useState('')
  const [configError, setConfigError] = useState<string | null>(null)
  const [isPending, setIsPending] = useState(false)

  useEffect(() => {
    if (open) {
      if (mcp) {
        setSlug(mcp.slug)
        setName(mcp.name)
        setDescription(mcp.description || '')
        setConfigText(JSON.stringify(mcp.config, null, 2))
      } else {
        setSlug('')
        setName('')
        setDescription('')
        setConfigText(MCP_CONFIG_EXAMPLE)
      }
      setConfigError(null)
    }
  }, [open, mcp])

  const handleNameChange = (value: string) => {
    setName(value)
    if (!isEdit) {
      setSlug(
        value
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, '-')
          .replace(/^-|-$/g, '')
      )
    }
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

  const handleOpenChange = (nextOpen: boolean) => {
    if (!nextOpen) {
      setConfigText('')
      setConfigError(null)
    }
    onOpenChange(nextOpen)
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim() || !slug.trim()) return

    let config: Record<string, unknown>
    try {
      config = JSON.parse(configText)
    } catch {
      setConfigError('Invalid JSON — please fix before saving')
      return
    }

    setIsPending(true)
    try {
      if (isEdit) {
        await updateMcp({
          path: { project_id: projectId, slug: mcp!.slug },
          body: {
            name: name.trim(),
            description: description.trim() || undefined,
            config,
          },
          throwOnError: true,
        })
        toast.success('MCP server updated')
      } else {
        await createMcp({
          path: { project_id: projectId },
          body: {
            slug: slug.trim(),
            name: name.trim(),
            description: description.trim() || undefined,
            config,
          },
          throwOnError: true,
        })
        toast.success('MCP server created')
      }
      setConfigText('')
      onSuccess()
    } catch (err) {
      toast.error(
        isEdit ? 'Failed to update MCP server' : 'Failed to create MCP server'
      )
    } finally {
      setIsPending(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? 'Edit MCP Server' : 'Create MCP Server'}
          </DialogTitle>
          <DialogDescription>
            {isEdit
              ? 'Update this MCP server configuration.'
              : 'Define a new MCP server that can be assigned to AI workflows.'}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="mcp-name">Name</Label>
              <Input
                id="mcp-name"
                value={name}
                onChange={(e) => handleNameChange(e.target.value)}
                placeholder="e.g. Filesystem Server"
                required
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="mcp-slug">Slug</Label>
              <Input
                id="mcp-slug"
                value={slug}
                onChange={(e) => setSlug(e.target.value)}
                placeholder="e.g. filesystem"
                disabled={isEdit}
                required
                className="font-mono"
              />
              {isEdit && (
                <p className="text-xs text-muted-foreground">
                  Slug cannot be changed after creation.
                </p>
              )}
            </div>
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
              MCP server config following the{' '}
              <code className="bg-muted px-1 rounded">mcpServers</code> format:
              command, args, and optional env vars.
            </p>
            <Textarea
              id="mcp-config"
              value={configText}
              onChange={(e) => handleConfigChange(e.target.value)}
              required
              className="font-mono text-sm min-h-[180px]"
              rows={10}
            />
            {configError && (
              <p className="text-xs text-destructive">{configError}</p>
            )}
          </div>
          {isEdit && (
            <McpCredentialRevealControls
              key={`${projectId}:${mcp!.slug}:${mcp!.updated_at}:${open}`}
              configText={configText}
              onConfigTextChange={handleConfigChange}
              onRevealCredential={async (field) => {
                const { data } = await revealMcpConfig({
                  path: {
                    project_id: projectId,
                    slug: mcp!.slug,
                    field,
                  },
                  throwOnError: true,
                })
                return data.value
              }}
            />
          )}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => handleOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={isPending || configError !== null}>
              {isPending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
              ) : null}
              {isPending
                ? isEdit
                  ? 'Saving...'
                  : 'Creating...'
                : isEdit
                  ? 'Save'
                  : 'Create MCP Server'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
