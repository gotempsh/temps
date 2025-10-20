import { AlertCircle } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'

interface ErrorAlertProps {
  title?: string
  description: string
  retry?: () => void
}

export function ErrorAlert({
  title = 'Error',
  description,
  retry,
}: ErrorAlertProps) {
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" />
      <AlertTitle>{title}</AlertTitle>
      <div className="flex items-center justify-between gap-4">
        <AlertDescription>{description}</AlertDescription>
        {retry && (
          <Button variant="destructive" size="sm" onClick={retry}>
            Try Again
          </Button>
        )}
      </div>
    </Alert>
  )
}
