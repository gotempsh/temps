import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { AlertTriangle } from 'lucide-react'
import type { ApiKeyResponse } from '@/api/client'

interface ApiKeyDeleteModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  apiKey: ApiKeyResponse | null
  onConfirm: (id: number) => void
  isPending: boolean
}

export function ApiKeyDeleteModal({
  open,
  onOpenChange,
  apiKey,
  onConfirm,
  isPending,
}: ApiKeyDeleteModalProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete API Key</DialogTitle>
          <DialogDescription>
            Are you sure you want to delete the API key &quot;{apiKey?.name}
            &quot;?
          </DialogDescription>
        </DialogHeader>
        <div className="py-4">
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertDescription>
              This action cannot be undone. Any applications using this API key
              will immediately lose access.
            </AlertDescription>
          </Alert>
          {apiKey && (
            <div className="mt-4 space-y-2 text-sm">
              <p>
                <strong>Key:</strong>{' '}
                <code className="bg-muted px-1 py-0.5 rounded">
                  {apiKey.key_prefix}...
                </code>
              </p>
              <p>
                <strong>Role:</strong> {apiKey.role_type}
              </p>
              <p>
                <strong>Created:</strong>{' '}
                {new Date(apiKey.created_at).toLocaleDateString()}
              </p>
              {apiKey.last_used_at && (
                <p>
                  <strong>Last used:</strong>{' '}
                  {new Date(apiKey.last_used_at).toLocaleDateString()}
                </p>
              )}
            </div>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            onClick={() => apiKey && onConfirm(apiKey.id)}
            disabled={isPending}
          >
            {isPending ? 'Deleting...' : 'Delete'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
