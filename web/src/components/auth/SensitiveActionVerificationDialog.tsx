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
import { verifyStepUp, type ProblemDetails } from '@/api/client'
import { canCloseSensitiveActionDialog } from '@/lib/sensitiveActionProblem'
import { AlertCircle, KeyRound, Loader2 } from 'lucide-react'
import { FormEvent, useState } from 'react'
import { Link } from 'react-router'

interface SensitiveActionVerificationDialogProps {
  open: boolean
  mfaSetupRequired: boolean
  onOpenChange: (open: boolean) => void
  onVerified: () => void
}

export function SensitiveActionVerificationDialog({
  open,
  mfaSetupRequired,
  onOpenChange,
  onVerified,
}: SensitiveActionVerificationDialogProps) {
  const [code, setCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [isVerifying, setIsVerifying] = useState(false)

  const handleOpenChange = (nextOpen: boolean) => {
    if (!nextOpen && !canCloseSensitiveActionDialog(isVerifying)) return
    if (!nextOpen) {
      setCode('')
      setError(null)
    }
    onOpenChange(nextOpen)
  }

  const verify = async (event: FormEvent) => {
    event.preventDefault()
    const trimmedCode = code.trim()
    if (!trimmedCode) {
      setError('Enter a TOTP code or recovery code.')
      return
    }

    setIsVerifying(true)
    setError(null)
    try {
      const response = await verifyStepUp({ body: { code: trimmedCode } })
      if (response.error) {
        const problem = response.error as ProblemDetails
        throw new Error(problem?.detail || 'Verification failed. Try again.')
      }
      setCode('')
      setError(null)
      onVerified()
    } catch (verificationError) {
      setError(
        verificationError instanceof Error
          ? verificationError.message
          : 'Verification failed. Try again.'
      )
    } finally {
      setIsVerifying(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (isVerifying) event.preventDefault()
        }}
        onPointerDownOutside={(event) => {
          if (isVerifying) event.preventDefault()
        }}
      >
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <KeyRound className="h-5 w-5" />
            Verify this sensitive action
          </DialogTitle>
          <DialogDescription>
            Confirm your identity before Temps continues. Verification applies
            only to this browser session and expires after five minutes.
          </DialogDescription>
        </DialogHeader>

        {mfaSetupRequired ? (
          <div className="space-y-4">
            <Alert variant="warning">
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Configure multi-factor authentication before performing
                sensitive actions.
              </AlertDescription>
            </Alert>
            <DialogFooter>
              <Button variant="outline" onClick={() => handleOpenChange(false)}>
                Cancel
              </Button>
              <Button asChild>
                <Link to="/account" onClick={() => handleOpenChange(false)}>
                  Configure MFA
                </Link>
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <form onSubmit={verify} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="sensitive-action-code">
                TOTP or recovery code
              </Label>
              <Input
                id="sensitive-action-code"
                value={code}
                onChange={(event) => setCode(event.target.value)}
                autoComplete="one-time-code"
                autoFocus
                disabled={isVerifying}
              />
            </div>
            {error && (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => handleOpenChange(false)}
                disabled={isVerifying}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={isVerifying}>
                {isVerifying && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Verify and continue
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
