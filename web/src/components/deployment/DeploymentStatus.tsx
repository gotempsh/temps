import { DeploymentResponse } from '@/api/client'
import { Loader2 } from 'lucide-react'
import { useState, useEffect } from 'react'

interface DeploymentStatusProps {
  deployment: DeploymentResponse
}

export function DeploymentStatus({ deployment }: DeploymentStatusProps) {
  const [elapsedTime, setElapsedTime] = useState<number>(0)

  useEffect(() => {
    let intervalId: ReturnType<typeof setInterval> | undefined

    if (deployment.status === 'running' && deployment.started_at) {
      // Calculate initial elapsed time
      const startTime = new Date(deployment.started_at).getTime()
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setElapsedTime(Math.round((Date.now() - startTime) / 1000))

      // Update elapsed time every second
      intervalId = setInterval(() => {
        setElapsedTime((prev) => prev + 1)
      }, 1000)
    }

    return () => {
      if (intervalId) {
        clearInterval(intervalId)
      }
    }
  }, [deployment.status, deployment.started_at])

  if (deployment.status === 'running') {
    return (
      <>
        <Loader2 className="h-3 w-3 animate-spin mr-1 inline-block" />
        {deployment.status} • {elapsedTime}s
      </>
    )
  }

  return (
    <>
      {deployment.status}
      {deployment.finished_at && deployment.started_at && (
        <>
          {' '}
          •{' '}
          {Math.round(
            (new Date(deployment.finished_at).getTime() -
              new Date(deployment.started_at).getTime()) /
              1000
          )}
          s
        </>
      )}
    </>
  )
}
