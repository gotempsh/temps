import { Alert, AlertDescription } from '@/components/ui/alert'
import { CheckCircle2, AlertCircle, Info, AlertTriangle } from 'lucide-react'
import { useEffect } from 'react'

export type FeedbackType = 'success' | 'error' | 'warning' | 'info'

export interface FeedbackMessage {
  type: FeedbackType
  message: string
}

interface FeedbackAlertProps {
  feedback: FeedbackMessage | null
  onDismiss?: () => void
  autoHideDelay?: number // in milliseconds
}

export function FeedbackAlert({
  feedback,
  onDismiss,
  autoHideDelay = 5000,
}: FeedbackAlertProps) {
  useEffect(() => {
    if (feedback && autoHideDelay > 0 && onDismiss) {
      const timer = setTimeout(() => {
        onDismiss()
      }, autoHideDelay)

      return () => clearTimeout(timer)
    }
  }, [feedback, autoHideDelay, onDismiss])

  if (!feedback) return null

  const getIcon = () => {
    switch (feedback.type) {
      case 'success':
        return <CheckCircle2 className="h-4 w-4" />
      case 'error':
        return <AlertCircle className="h-4 w-4" />
      case 'warning':
        return <AlertTriangle className="h-4 w-4" />
      case 'info':
        return <Info className="h-4 w-4" />
    }
  }

  const getVariant = () => {
    switch (feedback.type) {
      case 'error':
        return 'destructive'
      default:
        return 'default'
    }
  }

  return (
    <Alert variant={getVariant()}>
      {getIcon()}
      <AlertDescription>{feedback.message}</AlertDescription>
    </Alert>
  )
}
