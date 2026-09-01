// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { McpCredentialRevealControls } from '@/components/agents/McpCredentialRevealControls'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { Card, CardContent } from '@/components/ui/card'
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
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ChevronRight,
  EllipsisVertical,
  Loader2,
  Plus,
  Server,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import {
  createGlobalMcp,
  revealGlobalMcpConfig,
  updateGlobalMcp,
} from '@/api/client/sdk.gen'
import {
  deleteGlobalMcpMutation,
  listGlobalMcpsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { McpDefinitionResponse as McpDefinition } from '@/api/client/types.gen'

export function GlobalMcpServersSettings() {
  usePageTitle('MCP Servers')
  const navigate = useNavigate()

  const [mcpToDelete, setMcpToDelete] = useState<string | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingMcp, setEditingMcp] = useState<McpDefinition | null>(null)

  const {
    data: mcpData,
    isLoading,
    error,
    refetch,
  } = useQuery(listGlobalMcpsOptions())
  const mcpServers = mcpData?.items

  const deleteMutation = useMutation({
    ...deleteGlobalMcpMutation(),
    onSuccess: () => {
      toast.success('MCP server deleted')
      refetch()
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
          <h2 className="text-lg font-semibold">Global MCP Servers</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Platform-wide MCP server configurations available to all projects.
            Configs are merged into{' '}
            <code className="text-xs bg-muted px-1 rounded">
              .claude/settings.json
            </code>{' '}
            at workflow runtime.
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
      ) : !error && mcpServers && mcpServers.length > 0 ? (
        <McpCompactRows
          mcpServers={mcpServers}
          onOpen={(slug) => navigate(`/mcp-servers/${slug}`)}
          onEdit={openEdit}
          onDelete={setMcpToDelete}
        />
      ) : !error ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Server className="h-12 w-12 text-muted-foreground/50 mb-4" />
            <h3 className="text-lg font-semibold mb-2">
              No global MCP servers
            </h3>
            <p className="text-sm text-muted-foreground text-center mb-4 max-w-md">
              Global MCP servers are available to all projects. Define common
              tools like browser automation, database access, or file system
              servers here.
            </p>
            <Button onClick={openCreate}>
              <Plus className="h-4 w-4 mr-2" />
              Create Your First MCP Server
            </Button>
          </CardContent>
        </Card>
      ) : null}

      <GlobalMcpDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        mcp={editingMcp}
        onSuccess={() => {
          refetch()
          setDialogOpen(false)
        }}
      />

      <AlertDialog
        open={mcpToDelete !== null}
        onOpenChange={() => setMcpToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete global MCP server?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete this MCP server. All projects that
              reference it will lose access.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (mcpToDelete)
                  deleteMutation.mutate({ path: { slug: mcpToDelete } })
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

// ── Shared row menu ──

interface McpRowMenuProps {
  onView: () => void
  onEdit: () => void
  onDelete: () => void
}

function McpRowMenu({ onView, onEdit, onDelete }: McpRowMenuProps) {
  return (
    <div
      onClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className="h-8 w-8">
            <EllipsisVertical className="h-4 w-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem
            onSelect={(e) => {
              e.preventDefault()
              onView()
            }}
          >
            View details
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={(e) => {
              e.preventDefault()
              onEdit()
            }}
          >
            Edit
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            className="text-destructive"
            onSelect={(e) => {
              e.preventDefault()
              onDelete()
            }}
          >
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

// ── Variant: Compact rows ──

interface McpListProps {
  mcpServers: McpDefinition[]
  onOpen: (slug: string) => void
  onEdit: (mcp: McpDefinition) => void
  onDelete: (slug: string) => void
}

function McpCompactRows({
  mcpServers,
  onOpen,
  onEdit,
  onDelete,
}: McpListProps) {
  return (
    <div className="overflow-hidden rounded-lg border">
      <ul role="list" className="divide-y">
        {mcpServers.map((mcp) => (
          <li
            key={mcp.id}
            role="button"
            tabIndex={0}
            onClick={() => onOpen(mcp.slug)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                onOpen(mcp.slug)
              }
            }}
            className="flex cursor-pointer items-center gap-4 px-4 py-3 hover:bg-muted/40 transition-colors focus:outline-none focus:bg-muted/40"
          >
            <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
              <Server className="size-4 text-muted-foreground" />
            </div>
            <div className="flex min-w-0 flex-1 items-center gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <p className="truncate text-sm font-medium">{mcp.name}</p>
                  <Badge variant="secondary" className="font-mono text-xs">
                    {mcp.slug}
                  </Badge>
                </div>
                {mcp.description && (
                  <p className="mt-0.5 truncate text-xs text-muted-foreground">
                    {mcp.description}
                  </p>
                )}
              </div>
            </div>
            <McpRowMenu
              onView={() => onOpen(mcp.slug)}
              onEdit={() => onEdit(mcp)}
              onDelete={() => onDelete(mcp.slug)}
            />
            <ChevronRight className="size-4 shrink-0 text-muted-foreground/50" />
          </li>
        ))}
      </ul>
    </div>
  )
}

// ── Create / Edit Dialog ──

const MCP_CONFIG_EXAMPLE = `{
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"],
  "env": {}
}`

interface GlobalMcpDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  mcp: McpDefinition | null
  onSuccess: () => void
}

function GlobalMcpDialog({
  open,
  onOpenChange,
  mcp,
  onSuccess,
}: GlobalMcpDialogProps) {
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
        await updateGlobalMcp({
          path: { slug: mcp!.slug },
          body: {
            name: name.trim(),
            description: description.trim() || undefined,
            config,
          },
          throwOnError: true,
        })
        toast.success('MCP server updated')
      } else {
        await createGlobalMcp({
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
    } catch {
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
            {isEdit ? 'Edit Global MCP Server' : 'Create Global MCP Server'}
          </DialogTitle>
          <DialogDescription>
            {isEdit
              ? 'Update this global MCP server configuration.'
              : 'Define a new global MCP server available to all projects.'}
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
              key={`${mcp!.slug}:${mcp!.updated_at}:${open}`}
              configText={configText}
              onConfigTextChange={handleConfigChange}
              onRevealCredential={async (field) => {
                const { data } = await revealGlobalMcpConfig({
                  path: { slug: mcp!.slug, field },
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
