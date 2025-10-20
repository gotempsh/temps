import { AlertCircle } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'

export const SetupNotice = () => {
  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-4">
      <div className="mx-auto max-w-[450px] space-y-6">
        <Alert variant="default">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Application Not Configured</AlertTitle>
          <AlertDescription>
            The application has not been properly configured. Please contact
            your system administrator to complete the setup process.
          </AlertDescription>
        </Alert>
      </div>
    </div>
  )
}
