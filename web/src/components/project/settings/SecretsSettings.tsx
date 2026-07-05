import { ProjectResponse } from '@/api/client'
import {
  createProjectSecretMutation,
  deleteProjectSecretMutation,
  getEnvironmentsOptions,
  listProjectSecretsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { FileLock2, Plus, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

interface SecretsSettingsProps {
  project: ProjectResponse
}

// Conservative client-side mirror of the server's validation. The server is
// authoritative; this just lets us surface errors before a round-trip.
const MAX_SECRET_BYTES = 1_048_576 // 1 MiB
const KEY_PATTERN = /^[A-Za-z_][A-Za-z0-9_]{0,254}$/

export function SecretsSettings({ project }: SecretsSettingsProps) {
  const queryClient = useQueryClient()

  const secretsQuery = useQuery({
    ...listProjectSecretsOptions({
      path: { project_id: project.id },
      query: {},
    }),
  })

  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
  })

  const [isCreateOpen, setIsCreateOpen] = useState(false)

  // `n` opens the Create Secret dialog. Mirrors the env-vars page shortcut.
  // Skips when the user is typing in an input/textarea/contentEditable so
  // typing the literal "n" inside a form field doesn't hijack focus.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (
        e.key === 'n' &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.shiftKey &&
        !e.altKey
      ) {
        const target = e.target as HTMLElement
        if (
          target.tagName !== 'INPUT' &&
          target.tagName !== 'TEXTAREA' &&
          !target.isContentEditable
        ) {
          e.preventDefault()
          setIsCreateOpen(true)
        }
      }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [])

  const refetchSecrets = () => {
    queryClient.invalidateQueries({
      queryKey: listProjectSecretsOptions({
        path: { project_id: project.id },
        query: {},
      }).queryKey,
    })
  }

  const secrets = secretsQuery.data ?? []
  const environments = environmentsQuery.data ?? []

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <FileLock2 className="h-5 w-5" />
            Secrets
          </h2>
          <p className="text-sm text-muted-foreground max-w-2xl">
            Secrets are mounted into your containers as files at{' '}
            <code className="text-xs bg-muted px-1.5 py-0.5 rounded">
              /run/secrets/&lt;KEY&gt;
            </code>{' '}
            (mode 0400, tmpfs). They are never visible via{' '}
            <code className="text-xs bg-muted px-1.5 py-0.5 rounded">
              docker inspect
            </code>{' '}
            and never injected as environment variables. Read with e.g.{' '}
            <code className="text-xs bg-muted px-1.5 py-0.5 rounded">
              fs.readFileSync('/run/secrets/DB_PASSWORD', 'utf8')
            </code>
            . A redeploy is required for new or updated secrets to take effect.
          </p>
        </div>
        <Button onClick={() => setIsCreateOpen(true)}>
          <Plus className="h-4 w-4 mr-1" />
          New secret
          <KbdBadge keys={['N']} className="ml-2 hidden sm:inline-flex" />
        </Button>
      </div>

      {secretsQuery.isPending ? (
        <div className="space-y-2">
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      ) : secrets.length === 0 ? (
        <div className="border border-dashed rounded-md p-8 text-center text-sm text-muted-foreground">
          No secrets yet. Click <strong>New secret</strong> to add one.
        </div>
      ) : (
        <div className="border rounded-md divide-y">
          {secrets.map((s) => (
            <SecretRow
              key={s.id}
              secret={s}
              projectId={project.id}
              onDeleted={refetchSecrets}
            />
          ))}
        </div>
      )}

      <CreateSecretDialog
        open={isCreateOpen}
        onOpenChange={setIsCreateOpen}
        projectId={project.id}
        environments={environments}
        onCreated={refetchSecrets}
      />
    </div>
  )
}

interface SecretRowProps {
  secret: {
    id: number
    key: string
    include_in_preview: boolean
    created_at: number
    updated_at: number
    environments: Array<{ id: number; name: string; main_url: string }>
  }
  projectId: number
  onDeleted: () => void
}

function SecretRow({ secret, projectId, onDeleted }: SecretRowProps) {
  const deleteMutation = useMutation({
    ...deleteProjectSecretMutation(),
    onSuccess: () => {
      toast.success(`Secret ${secret.key} deleted. Redeploy to take effect.`)
      onDeleted()
    },
    onError: (err: Error) => {
      toast.error(
        err instanceof Error ? err.message : 'Failed to delete secret',
      )
    },
  })

  return (
    <div className="flex items-center gap-3 px-4 py-3">
      <FileLock2 className="h-4 w-4 text-muted-foreground shrink-0" />
      <div className="min-w-0 flex-1">
        <div className="font-mono text-sm truncate">{secret.key}</div>
        <div className="text-xs text-muted-foreground">
          {secret.environments.length === 0
            ? 'All environments'
            : secret.environments.map((e) => e.name).join(', ')}
          {secret.include_in_preview ? ' • preview enabled' : ''}
        </div>
      </div>
      <span className="text-xs text-muted-foreground font-mono">••••••••</span>
      <AlertDialog>
        <AlertDialogTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="text-muted-foreground hover:text-destructive"
            aria-label={`Delete secret ${secret.key}`}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </AlertDialogTrigger>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete secret {secret.key}?</AlertDialogTitle>
            <AlertDialogDescription>
              Running containers keep their mounted secret file until the next
              deployment. New deployments will not receive this secret.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                deleteMutation.mutate({
                  path: { project_id: projectId, secret_id: secret.id },
                })
              }
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

interface CreateSecretDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  environments: Array<{ id: number; name: string }>
  onCreated: () => void
}

// Default-select environments whose name matches production or preview.
// Matches case-insensitively so "Production", "PROD", "Preview" all hit.
function defaultEnvironmentSelection(
  environments: Array<{ id: number; name: string }>,
): number[] {
  return environments
    .filter((e) => {
      const n = e.name.toLowerCase()
      return n.includes('prod') || n.includes('preview')
    })
    .map((e) => e.id)
}

function CreateSecretDialog({
  open,
  onOpenChange,
  projectId,
  environments,
  onCreated,
}: CreateSecretDialogProps) {
  const [key, setKey] = useState('')
  const [value, setValue] = useState('')
  const [environmentIds, setEnvironmentIds] = useState<number[]>(() =>
    defaultEnvironmentSelection(environments),
  )
  const [includeInPreview, setIncludeInPreview] = useState(false)
  const [keyError, setKeyError] = useState<string | null>(null)
  const [valueError, setValueError] = useState<string | null>(null)

  // Re-sync the default selection when the environments list resolves after
  // the dialog has already mounted. Only applies before the user has touched
  // the selection — once they've made an explicit choice, leave it alone.
  const [hasUserEditedEnvs, setHasUserEditedEnvs] = useState(false)
  useEffect(() => {
    if (!hasUserEditedEnvs) {
      setEnvironmentIds(defaultEnvironmentSelection(environments))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [environments])

  const createMutation = useMutation({
    ...createProjectSecretMutation(),
    onSuccess: () => {
      toast.success(
        `Secret ${key} created. Redeploy to mount it at /run/secrets/${key}.`,
      )
      onCreated()
      setKey('')
      setValue('')
      setEnvironmentIds(defaultEnvironmentSelection(environments))
      setHasUserEditedEnvs(false)
      setIncludeInPreview(false)
      setKeyError(null)
      setValueError(null)
      onOpenChange(false)
    },
    onError: (err: Error) => {
      toast.error(
        err instanceof Error ? err.message : 'Failed to create secret',
      )
    },
  })

  const validateAndSubmit = () => {
    let ok = true
    if (!KEY_PATTERN.test(key)) {
      setKeyError(
        'Must start with a letter or underscore and contain only A-Z, a-z, 0-9, _',
      )
      ok = false
    } else {
      setKeyError(null)
    }
    if (new TextEncoder().encode(value).length > MAX_SECRET_BYTES) {
      setValueError(`Value exceeds 1 MiB limit`)
      ok = false
    } else {
      setValueError(null)
    }
    if (!ok) return
    createMutation.mutate({
      path: { project_id: projectId },
      body: {
        key,
        value,
        environment_ids: environmentIds,
        include_in_preview: includeInPreview,
      },
    })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Create secret</DialogTitle>
          <DialogDescription>
            The value is encrypted at rest and cannot be retrieved after save.
            Save it in your password manager if you need a copy.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label htmlFor="secret-key">Key</Label>
            <Input
              id="secret-key"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder="DB_PASSWORD"
              className="font-mono"
              autoFocus
            />
            {keyError ? (
              <p className="text-xs text-destructive mt-1">{keyError}</p>
            ) : (
              <p className="text-xs text-muted-foreground mt-1">
                Becomes the filename at /run/secrets/&lt;KEY&gt;
              </p>
            )}
          </div>
          <div>
            <Label htmlFor="secret-value">Value</Label>
            <Textarea
              id="secret-value"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              placeholder="Plaintext value (max 1 MiB)"
              className="font-mono text-xs"
              rows={4}
            />
            {valueError && (
              <p className="text-xs text-destructive mt-1">{valueError}</p>
            )}
          </div>
          <div>
            <Label>Environments</Label>
            <div className="mt-2 space-y-2">
              {environments.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  No environments yet — secret will apply to all environments
                  once created.
                </p>
              ) : (
                environments.map((env) => (
                  <label
                    key={env.id}
                    className="flex items-center gap-2 text-sm cursor-pointer"
                  >
                    <Checkbox
                      checked={environmentIds.includes(env.id)}
                      onCheckedChange={(checked) => {
                        setHasUserEditedEnvs(true)
                        setEnvironmentIds((prev) =>
                          checked
                            ? [...prev, env.id]
                            : prev.filter((id) => id !== env.id),
                        )
                      }}
                    />
                    {env.name}
                  </label>
                ))
              )}
              <p className="text-xs text-muted-foreground">
                Leave empty to apply to all environments.
              </p>
            </div>
          </div>
          <label className="flex items-center gap-2 text-sm cursor-pointer">
            <Checkbox
              checked={includeInPreview}
              onCheckedChange={(checked) => setIncludeInPreview(!!checked)}
            />
            Include in preview environments
          </label>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            onClick={validateAndSubmit}
            disabled={!key || !value || createMutation.isPending}
          >
            {createMutation.isPending ? 'Saving…' : 'Create secret'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
