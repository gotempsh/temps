import * as React from 'react'

interface StructuredAiEvent<T> {
  type: 'started' | 'partial' | 'complete' | 'error'
  cached?: boolean
  provider?: string
  model?: string
  thinking_level?: string
  value?: Partial<T>
  message?: string
}

export function parseStructuredSseFrame(frame: string): unknown | null {
  const payload = frame
    .split(/\r?\n/)
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).trimStart())
    .join('\n')
  return payload ? JSON.parse(payload) : null
}

/**
 * Reusable client for Temps' provider-neutral structured AI endpoint. Screens
 * own only their prompt, JSON Schema, and field-level skeletons; provider
 * routing, validation, caching, refresh, and incremental JSON stay generic.
 */
export function useStructuredAi<T extends object>({
  projectId,
  purpose,
  schema,
  prompt,
  cacheKey,
  providerKeyId,
  model,
  thinkingLevel,
  cacheTtlSeconds = 60,
  refreshNonce = 0,
  enabled = true,
}: {
  projectId: number
  purpose: string
  schema: object
  prompt: string | null
  cacheKey?: string
  providerKeyId?: number
  model?: string
  thinkingLevel?: string
  cacheTtlSeconds?: number
  refreshNonce?: number
  enabled?: boolean
}) {
  const [value, setValue] = React.useState<Partial<T>>()
  const [error, setError] = React.useState<string>()
  const [failureStatus, setFailureStatus] = React.useState<number>()
  const [isPending, setIsPending] = React.useState(false)
  const [cached, setCached] = React.useState(false)
  const [resolved, setResolved] = React.useState<{
    provider: string
    model: string
    thinkingLevel?: string
  }>()
  const previousRefreshNonce = React.useRef(refreshNonce)

  React.useEffect(() => {
    let active = true
    if (!enabled || !prompt) {
      queueMicrotask(() => {
        if (!active) return
        setValue(undefined)
        setError(undefined)
        setFailureStatus(undefined)
        setIsPending(false)
        setResolved(undefined)
      })
      return () => {
        active = false
      }
    }
    const controller = new AbortController()
    const forceRefresh = refreshNonce !== previousRefreshNonce.current
    previousRefreshNonce.current = refreshNonce
    queueMicrotask(() => {
      if (!active) return
      setValue(undefined)
      setError(undefined)
      setFailureStatus(undefined)
      setCached(false)
      setResolved(undefined)
      setIsPending(true)
    })

    void (async () => {
      try {
        const response = await fetch(
          `/api/projects/${projectId}/ai/structured-output/stream`,
          {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            signal: controller.signal,
            body: JSON.stringify({
              purpose,
              prompt,
              schema,
              provider_key_id: providerKeyId,
              model,
              thinking_level: thinkingLevel,
              cache_key: cacheKey,
              cache_ttl_seconds: cacheTtlSeconds,
              refresh: forceRefresh,
            }),
          }
        )
        if (!response.ok || !response.body) {
          const problem = await response.json().catch(() => null)
          if (active) setFailureStatus(response.status)
          throw new Error(
            problem?.detail ??
              `Structured AI request failed (${response.status})`
          )
        }

        const reader = response.body.getReader()
        const decoder = new TextDecoder()
        let buffer = ''
        while (active) {
          const { done, value: chunk } = await reader.read()
          buffer += decoder.decode(chunk, { stream: !done })
          const frames = buffer.split(/\r?\n\r?\n/)
          buffer = frames.pop() ?? ''
          for (const frame of frames) {
            const event = parseStructuredSseFrame(
              frame
            ) as StructuredAiEvent<T> | null
            if (!event) continue
            if (event.type === 'started') {
              setCached(Boolean(event.cached))
              if (event.provider && event.model) {
                setResolved({
                  provider: event.provider,
                  model: event.model,
                  thinkingLevel: event.thinking_level,
                })
              }
            }
            if (event.type === 'partial' || event.type === 'complete') {
              setValue(event.value)
            }
            if (event.type === 'error') {
              throw new Error(event.message ?? 'Structured AI stream failed')
            }
            if (event.type === 'complete') setIsPending(false)
          }
          if (done) break
        }
        if (active) setIsPending(false)
      } catch (cause) {
        if (!controller.signal.aborted && active) {
          setError(cause instanceof Error ? cause.message : String(cause))
          setIsPending(false)
        }
      }
    })()

    return () => {
      active = false
      controller.abort()
    }
  }, [
    cacheKey,
    cacheTtlSeconds,
    enabled,
    projectId,
    prompt,
    providerKeyId,
    model,
    thinkingLevel,
    purpose,
    refreshNonce,
    schema,
  ])

  return { value, error, failureStatus, isPending, cached, resolved }
}
