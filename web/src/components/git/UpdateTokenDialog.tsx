import { updateConnectionTokenMutation } from '@/api/client/@tanstack/react-query.gen'
import { ProblemDetails } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
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
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, Key, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

interface UpdateTokenDialogProps {
  connectionId: number
  connectionName: string
  open: boolean
  onOpenChange: (open: boolean) => void
}

interface TokenFormData {
  access_token: string
  refresh_token?: string
}

export function UpdateTokenDialog({
  connectionId,
  connectionName,
  open,
  onOpenChange,
}: UpdateTokenDialogProps) {
  const queryClient = useQueryClient()

  const {
    register,
    handleSubmit,
    formState: { isDirty, isSubmitting, errors },
    reset,
    setError,
  } = useForm<TokenFormData>({
    defaultValues: {
      access_token: '',
      refresh_token: '',
    },
  })

  const updateTokenMutation = useMutation({
    ...updateConnectionTokenMutation(),
    meta: {
      errorTitle: 'Failed to update connection token',
    },
    onSuccess: (data) => {
      console.log('data', data)
      toast.success(data?.message || 'Token updated successfully')
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      reset()
      onOpenChange(false)
    },
    onError: (error) => {
      const problemDetails = error as unknown as ProblemDetails

      // Check for invalid token error and set field error
      if (problemDetails?.detail?.toLowerCase().includes('invalid token')) {
        setError('access_token', {
          type: 'manual',
          message: problemDetails.detail || 'Invalid access token',
        })
      }
    },
  })

  const onSubmit = (data: TokenFormData) => {
    updateTokenMutation.mutate({
      path: { connection_id: connectionId },
      body: {
        access_token: data.access_token,
        refresh_token: data.refresh_token || null,
      },
    })
  }

  const handleClose = () => {
    if (!isSubmitting) {
      reset()
      onOpenChange(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Key className="h-5 w-5" />
            Update Access Token
          </DialogTitle>
          <DialogDescription>
            Update the access token for <strong>{connectionName}</strong>
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit(onSubmit)} className="space-y-4">
          {/* Display API error if present */}
          {updateTokenMutation.error &&
            (() => {
              const problemDetails =
                updateTokenMutation.error as unknown as ProblemDetails
              const errorMessage =
                problemDetails?.detail ||
                problemDetails?.title ||
                'An error occurred while updating the token'

              return (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>{errorMessage}</AlertDescription>
                </Alert>
              )
            })()}

          <div className="space-y-2">
            <Label htmlFor="access_token">
              Access Token <span className="text-destructive">*</span>
            </Label>
            <Input
              id="access_token"
              type="password"
              placeholder="ghp_xxxxxxxxxxxx"
              {...register('access_token', {
                required: 'Access token is required',
                minLength: {
                  value: 10,
                  message: 'Access token must be at least 10 characters',
                },
                validate: {
                  notEmpty: (value) =>
                    value.trim().length > 0 || 'Access token cannot be empty',
                },
              })}
              disabled={isSubmitting}
              className={errors.access_token ? 'border-destructive' : ''}
            />
            {errors.access_token ? (
              <p className="text-sm text-destructive">
                {errors.access_token.message}
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">
                Enter the new personal access token for this connection
              </p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="refresh_token">Refresh Token (Optional)</Label>
            <Input
              id="refresh_token"
              type="password"
              placeholder="ghr_xxxxxxxxxxxx"
              {...register('refresh_token')}
              disabled={isSubmitting}
            />
            <p className="text-sm text-muted-foreground">
              Optional refresh token if your provider supports it
            </p>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={handleClose}
              disabled={isSubmitting}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!isDirty || isSubmitting}>
              {isSubmitting ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Updating...
                </>
              ) : (
                <>
                  <Key className="mr-2 h-4 w-4" />
                  Update Token
                </>
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
