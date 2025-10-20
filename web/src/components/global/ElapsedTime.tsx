interface ElapsedTimeProps {
  startedAt: number
  endedAt?: number | null
}

import { useEffect, useState } from 'react'

export function ElapsedTime({ startedAt, endedAt }: ElapsedTimeProps) {
  if (!startedAt) return null

  const [now, setNow] = useState(() => Date.now())

  useEffect(() => {
    if (endedAt) return
    const interval = setInterval(() => {
      setNow(Date.now())
    }, 1000)
    return () => clearInterval(interval)
  }, [endedAt])

  const startTime = new Date(startedAt).getTime()
  const endTime = endedAt ? new Date(endedAt).getTime() : now
  const elapsedSeconds = Math.max(0, Math.floor((endTime - startTime) / 1000))

  return <span>{elapsedSeconds}s</span>
}
