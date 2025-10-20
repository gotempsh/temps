import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import { ScrollArea } from '@/components/ui/scroll-area'
import type { ApiKeyResponse, UpdateApiKeyRequest } from '@/api/client'

interface ApiKeyEditModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  apiKey: ApiKeyResponse | null
  onSubmit: (id: number, data: UpdateApiKeyRequest) => void
  isPending: boolean
}

export function ApiKeyEditModal({
  open,
  onOpenChange,
  apiKey,
  onSubmit,
  isPending,
}: ApiKeyEditModalProps) {
  const handleSubmit = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    if (!apiKey) return

    const formData = new FormData(e.currentTarget)
    const data: UpdateApiKeyRequest = {
      name: formData.get('name') as string,
      is_active: formData.get('is_active') === 'on',
      expires_at: formData.get('expires_at')
        ? new Date(formData.get('expires_at') as string).toISOString()
        : null,
    }
    onSubmit(apiKey.id, data)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[550px]">
        <DialogHeader>
          <DialogTitle>Edit API Key</DialogTitle>
          <DialogDescription>
            Update the details of your API key. Note that permissions cannot be
            changed after creation.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit}>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="edit-name">Name</Label>
              <Input
                id="edit-name"
                name="name"
                defaultValue={apiKey?.name}
                required
              />
            </div>

            <div className="flex items-center space-x-2">
              <Switch
                id="edit-is_active"
                name="is_active"
                defaultChecked={apiKey?.is_active}
              />
              <Label htmlFor="edit-is_active">Active</Label>
            </div>

            <div className="space-y-2">
              <Label htmlFor="edit-expires_at">
                Expiration Date (optional)
              </Label>
              <Input
                id="edit-expires_at"
                name="expires_at"
                type="date"
                defaultValue={
                  apiKey?.expires_at
                    ? new Date(apiKey.expires_at).toISOString().split('T')[0]
                    : ''
                }
                min={new Date().toISOString().split('T')[0]}
              />
            </div>

            {apiKey && (
              <>
                <div className="space-y-2">
                  <Label>Key Information</Label>
                  <div className="text-sm text-muted-foreground space-y-1">
                    <p>
                      Prefix:{' '}
                      <code className="bg-muted px-1 py-0.5 rounded">
                        {apiKey.key_prefix}...
                      </code>
                    </p>
                    <p>
                      Role: <Badge variant="outline">{apiKey.role_type}</Badge>
                    </p>
                    <p>
                      Created:{' '}
                      {new Date(apiKey.created_at).toLocaleDateString()}
                    </p>
                    {apiKey.last_used_at && (
                      <p>
                        Last used:{' '}
                        {new Date(apiKey.last_used_at).toLocaleDateString()}
                      </p>
                    )}
                  </div>
                </div>

                {apiKey.permissions && apiKey.permissions.length > 0 && (
                  <div className="space-y-2">
                    <Label>Permissions</Label>
                    <ScrollArea className="h-[150px] border rounded-md p-3">
                      <div className="flex flex-wrap gap-1">
                        {apiKey.permissions.map((permission) => (
                          <Badge
                            key={permission}
                            variant="secondary"
                            className="text-xs"
                          >
                            {permission}
                          </Badge>
                        ))}
                      </div>
                    </ScrollArea>
                    <p className="text-xs text-muted-foreground">
                      {apiKey.permissions.length} permission
                      {apiKey.permissions.length !== 1 ? 's' : ''} assigned
                    </p>
                  </div>
                )}
              </>
            )}
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending ? 'Saving...' : 'Save'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
