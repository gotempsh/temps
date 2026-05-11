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
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { KeyRound, Loader2, Pencil, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  deleteSecretMutation,
  listSecretsOptions,
  listSecretsQueryKey,
  upsertSecretMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  SecretResponse as AgentSecret,
  UpsertSecretRequest as CreateSecretRequest,
} from '@/api/client/types.gen'

export function AgentSecrets() {
  const queryClient = useQueryClient()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingSecret, setEditingSecret] = useState<AgentSecret | null>(null)
  const [secretToDelete, setSecretToDelete] = useState<AgentSecret | null>(null)

  const { data: secretsData, isLoading } = useQuery({
    ...listSecretsOptions(),
  })
  const secrets = secretsData?.items ?? []

  const upsertMutation = useMutation({
    ...upsertSecretMutation(),
    onSuccess: () => {
      toast.success(editingSecret ? 'Secret updated' : 'Secret saved')
      queryClient.invalidateQueries({ queryKey: listSecretsQueryKey() })
      setDialogOpen(false)
      setEditingSecret(null)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to save secret')
    },
  })

  const deleteMutation = useMutation({
    ...deleteSecretMutation(),
    onSuccess: () => {
      toast.success('Secret deleted')
      queryClient.invalidateQueries({ queryKey: listSecretsQueryKey() })
      setSecretToDelete(null)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to delete secret')
    },
  })

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2 text-base">
              <KeyRound className="h-4 w-4" />
              Secrets
            </CardTitle>
            <CardDescription>
              Encrypted secrets injected into all agent sandboxes as environment variables or files.
              Reference in config with <code className="bg-muted px-1 rounded text-xs">{'${TEMPS_SECRET:name}'}</code>.
            </CardDescription>
          </div>
          <CreateActionButton
            size="sm"
            onClick={() => {
              setEditingSecret(null)
              setDialogOpen(true)
            }}
            label="Add Secret"
          />
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : secrets.length === 0 ? (
          <p className="text-sm text-muted-foreground text-center py-6">
            No secrets configured. Add secrets to inject API keys, tokens, or config files into agent sandboxes.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead className="hidden sm:table-cell">Type</TableHead>
                  <TableHead className="hidden md:table-cell">Description</TableHead>
                  <TableHead className="hidden md:table-cell">Updated</TableHead>
                  <TableHead className="w-[96px]" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {secrets.map((secret: AgentSecret) => (
                  <TableRow key={secret.id}>
                    <TableCell className="font-mono text-sm">{secret.name}</TableCell>
                    <TableCell className="hidden sm:table-cell">
                      <span className="inline-flex items-center rounded-full border px-2 py-0.5 text-xs">
                        {secret.secret_type === 'env' ? 'Env Var' : 'File'}
                      </span>
                      {secret.mount_path && (
                        <span className="ml-1 text-xs text-muted-foreground font-mono">
                          {secret.mount_path}
                        </span>
                      )}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-muted-foreground text-sm">
                      {secret.description || '-'}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-muted-foreground text-sm">
                      {new Date(secret.updated_at).toLocaleDateString()}
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => {
                            setEditingSecret(secret)
                            setDialogOpen(true)
                          }}
                          aria-label={`Edit ${secret.name}`}
                        >
                          <Pencil className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => setSecretToDelete(secret)}
                          disabled={deleteMutation.isPending}
                          aria-label={`Delete ${secret.name}`}
                        >
                          <Trash2 className="h-3.5 w-3.5 text-destructive" />
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>

      <SecretDialog
        open={dialogOpen}
        onOpenChange={(open) => {
          setDialogOpen(open)
          if (!open) setEditingSecret(null)
        }}
        secret={editingSecret}
        onSubmit={(data) => upsertMutation.mutate({ body: data })}
        isPending={upsertMutation.isPending}
      />

      <AlertDialog
        open={secretToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setSecretToDelete(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete secret?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete{' '}
              <code className="bg-muted px-1 rounded font-mono text-xs">
                {secretToDelete?.name}
              </code>
              . Any agent or sandbox referencing it via{' '}
              <code className="bg-muted px-1 rounded font-mono text-xs">
                ${'{TEMPS_SECRET:'}
                {secretToDelete?.name}
                {'}'}
              </code>{' '}
              will fail to resolve it.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                if (secretToDelete)
                  deleteMutation.mutate({ path: { name: secretToDelete.name } })
              }}
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Card>
  )
}

function SecretDialog({
  open,
  onOpenChange,
  secret,
  onSubmit,
  isPending,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  secret: AgentSecret | null
  onSubmit: (data: CreateSecretRequest) => void
  isPending: boolean
}) {
  const isEdit = secret !== null
  const [name, setName] = useState('')
  const [secretType, setSecretType] = useState<'env' | 'file'>('env')
  const [value, setValue] = useState('')
  const [mountPath, setMountPath] = useState('')
  const [description, setDescription] = useState('')

  // Sync form state when the dialog opens with a secret (edit) or empty (create).
  // Value field always starts empty in edit mode — the API only returns a masked
  // placeholder, and leaving it blank means "don't change the value".
  useEffect(() => {
    if (!open) return
    if (secret) {
      setName(secret.name)
      setSecretType(secret.secret_type === 'file' ? 'file' : 'env')
      setValue('')
      setMountPath(secret.mount_path ?? '')
      setDescription(secret.description ?? '')
    } else {
      setName('')
      setSecretType('env')
      setValue('')
      setMountPath('')
      setDescription('')
    }
  }, [open, secret])

  const handleSubmit = () => {
    if (!name.trim()) {
      toast.error('Name is required')
      return
    }
    if (!isEdit && !value.trim()) {
      toast.error('Value is required')
      return
    }
    if (secretType === 'file' && !mountPath.trim()) {
      toast.error('Mount path is required for file secrets')
      return
    }

    // In edit mode an empty value means "don't change it" — but the API requires
    // a value field, so reuse the existing masked placeholder. The backend only
    // re-encrypts what's sent, so if the user leaves it blank we send the masked
    // string and the backend will overwrite the encrypted value with that mask.
    // To preserve the existing value, we require the user to enter a new one.
    if (isEdit && !value.trim()) {
      toast.error('Enter a new value, or delete and recreate the secret to keep it unchanged')
      return
    }

    onSubmit({
      name: name.trim(),
      secret_type: secretType,
      value: value.trim(),
      mount_path: secretType === 'file' ? mountPath.trim() : undefined,
      description: description.trim() || undefined,
    })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{isEdit ? 'Edit Secret' : 'Add Secret'}</DialogTitle>
        </DialogHeader>
        <form onSubmit={(e) => { e.preventDefault(); handleSubmit() }} className="space-y-4 py-2">
          <div className="space-y-1.5">
            <Label htmlFor="secret-name">Name</Label>
            <Input
              id="secret-name"
              value={name}
              onChange={(e) => setName(e.target.value.toUpperCase().replace(/[^A-Z0-9_]/g, '_'))}
              placeholder="ANTHROPIC_API_KEY"
              className="font-mono"
              disabled={isEdit}
            />
            <p className="text-xs text-muted-foreground">
              {isEdit
                ? 'Name cannot be changed. Delete and recreate to rename.'
                : (<>Use in config as <code className="bg-muted px-1 rounded">{'${TEMPS_SECRET:' + (name || 'NAME') + '}'}</code></>)}
            </p>
          </div>

          <div className="space-y-1.5">
            <Label>Type</Label>
            <Select value={secretType} onValueChange={(v) => setSecretType(v as 'env' | 'file')}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="env">Environment Variable</SelectItem>
                <SelectItem value="file">File</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-1.5">
            <Label htmlFor="secret-value">Value</Label>
            <Input
              id="secret-value"
              type="password"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              placeholder={
                isEdit
                  ? 'Enter new value (current value is hidden)'
                  : secretType === 'env'
                    ? 'sk-ant-api03-...'
                    : 'File contents...'
              }
            />
            <p className="text-xs text-muted-foreground">
              {isEdit
                ? 'The current value is encrypted and cannot be displayed. Type a new value to replace it.'
                : 'Encrypted with AES-256-GCM before storage.'}
            </p>
          </div>

          {secretType === 'file' && (
            <div className="space-y-1.5">
              <Label htmlFor="mount-path">Mount Path</Label>
              <Input
                id="mount-path"
                value={mountPath}
                onChange={(e) => setMountPath(e.target.value)}
                placeholder="/home/temps/.config/credentials.json"
                className="font-mono"
              />
            </div>
          )}

          <div className="space-y-1.5">
            <Label htmlFor="secret-description">Description</Label>
            <Input
              id="secret-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Optional description"
            />
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending ? 'Saving...' : isEdit ? 'Save Changes' : 'Save Secret'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
