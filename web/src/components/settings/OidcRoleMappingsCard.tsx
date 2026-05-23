import {
  createOidcRoleMappingMutation,
  deleteOidcRoleMappingMutation,
  listOidcRoleMappingsOptions,
  listOidcRoleMappingsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { OidcRoleMappingResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowRight, Loader2, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'

function nextDefaultPriority(existing: OidcRoleMappingResponse[]): number {
  if (existing.length === 0) return 100
  return Math.max(...existing.map((mapping) => mapping.priority)) + 10
}

export function OidcRoleMappingsCard({
  providerId,
  defaultRole,
}: {
  providerId: number
  defaultRole: string
}) {
  const queryClient = useQueryClient()
  const queryKey = listOidcRoleMappingsQueryKey({
    path: { provider_id: providerId },
  })
  const mappingsQuery = useQuery(
    listOidcRoleMappingsOptions({ path: { provider_id: providerId } }),
  )
  const mappings = mappingsQuery.data ?? []

  const [draftGroup, setDraftGroup] = useState('')
  const [draftRole, setDraftRole] = useState<'admin' | 'user'>('user')
  const [draftPriority, setDraftPriority] = useState(100)

  const suggestedPriority = useMemo(
    () => nextDefaultPriority(mappings),
    [mappings],
  )

  const createMapping = useMutation({
    ...createOidcRoleMappingMutation(),
    onSuccess: async () => {
      toast.success(`Added rule: ${draftGroup} → ${draftRole}`)
      setDraftGroup('')
      setDraftPriority(nextDefaultPriority(mappings) + 10)
      await queryClient.invalidateQueries({ queryKey })
    },
    onError: (error) => {
      toast.error(error instanceof Error ? error.message : 'Failed to add rule')
    },
  })

  const deleteMapping = useMutation({
    ...deleteOidcRoleMappingMutation(),
    onSuccess: async () => {
      toast.success('Rule removed')
      await queryClient.invalidateQueries({ queryKey })
    },
    onError: (error) => {
      toast.error(
        error instanceof Error ? error.message : 'Failed to delete rule',
      )
    },
  })

  const handleAdd = () => {
    const idpGroup = draftGroup.trim()
    if (!idpGroup) return
    createMapping.mutate({
      path: { provider_id: providerId },
      body: {
        idp_group: idpGroup,
        priority: draftPriority || suggestedPriority,
        role: draftRole,
      },
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Group → role mapping</CardTitle>
        <CardDescription>
          IdP groups from the configured group claim are matched in priority
          order; first match wins. Use <code className="rounded bg-muted px-1">*</code>{' '}
          as a fallback for any user. Unmatched users fall back to the provider
          default role ({defaultRole}).
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {mappingsQuery.isLoading ? (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Loading role mappings...
          </div>
        ) : mappings.length === 0 ? (
          <div className="rounded-md border p-3 text-sm text-muted-foreground">
            No rules yet. Users will receive the default role ({defaultRole})
            unless the IdP sends a matching roles claim.
          </div>
        ) : (
          <div className="space-y-2">
            {mappings.map((mapping) => (
              <div
                key={mapping.id}
                className="flex items-center gap-2 rounded-lg border p-2"
              >
                <span className="w-12 text-xs text-muted-foreground">
                  #{mapping.priority}
                </span>
                <code className="flex-1 rounded bg-muted px-2 py-1 font-mono text-xs">
                  {mapping.idp_group}
                </code>
                <ArrowRight className="h-4 w-4 text-muted-foreground" />
                <span className="w-20 rounded bg-primary/10 px-2 py-1 text-center font-mono text-xs text-primary">
                  {mapping.role}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={deleteMapping.isPending}
                  onClick={() => {
                    if (
                      !confirm(
                        `Remove rule "${mapping.idp_group} → ${mapping.role}"?`,
                      )
                    ) {
                      return
                    }
                    deleteMapping.mutate({
                      path: { mapping_id: mapping.id },
                    })
                  }}
                >
                  <Trash2 className="h-4 w-4 text-destructive" />
                </Button>
              </div>
            ))}
          </div>
        )}

        <div className="space-y-3 border-t pt-4">
          <div className="grid items-end gap-2 md:grid-cols-[auto_1fr_auto_auto]">
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground">Priority</label>
              <Input
                type="number"
                className="w-24"
                value={draftPriority}
                onChange={(event) =>
                  setDraftPriority(Number.parseInt(event.target.value || '100', 10))
                }
              />
            </div>
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground">IdP group</label>
              <Input
                value={draftGroup}
                onChange={(event) => setDraftGroup(event.target.value)}
                placeholder="temps-admins (or * for any)"
              />
            </div>
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground">Role</label>
              <Select
                value={draftRole}
                onValueChange={(value: 'admin' | 'user') => setDraftRole(value)}
              >
                <SelectTrigger className="w-28">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="admin">admin</SelectItem>
                  <SelectItem value="user">user</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <Button
              onClick={handleAdd}
              disabled={createMapping.isPending || !draftGroup.trim()}
            >
              <Plus className="mr-2 h-4 w-4" />
              Add
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
