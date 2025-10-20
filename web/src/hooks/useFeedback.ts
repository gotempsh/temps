import { useState, useCallback } from 'react'
import { FeedbackMessage, FeedbackType } from '@/components/ui/feedback-alert'

export function useFeedback() {
  const [feedback, setFeedback] = useState<FeedbackMessage | null>(null)

  const showFeedback = useCallback((type: FeedbackType, message: string) => {
    setFeedback({ type, message })
  }, [])

  const clearFeedback = useCallback(() => {
    setFeedback(null)
  }, [])

  const showSuccess = useCallback(
    (message: string) => {
      showFeedback('success', message)
    },
    [showFeedback]
  )

  const showError = useCallback(
    (message: string) => {
      showFeedback('error', message)
    },
    [showFeedback]
  )

  const showWarning = useCallback(
    (message: string) => {
      showFeedback('warning', message)
    },
    [showFeedback]
  )

  const showInfo = useCallback(
    (message: string) => {
      showFeedback('info', message)
    },
    [showFeedback]
  )

  return {
    feedback,
    showFeedback,
    clearFeedback,
    showSuccess,
    showError,
    showWarning,
    showInfo,
  }
}
