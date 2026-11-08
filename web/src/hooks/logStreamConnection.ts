// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

type Status = 'connecting' | 'connected' | 'complete' | 'error'

// Keep ownership of callbacks and timers together: a socket closed by an
// effect cleanup must never overwrite the state of its replacement.
export function connectLogStream(
  create: () => WebSocket,
  status: (state: Status, message?: string) => void,
  message: (event: MessageEvent) => void,
  schedule: typeof setTimeout = setTimeout,
  cancel: typeof clearTimeout = clearTimeout
): () => void {
  let disposed = false
  let socket: WebSocket | undefined
  let timer: ReturnType<typeof setTimeout> | undefined
  let failures = 0
  const connect = () => {
    if (disposed) return
    status('connecting')
    let received = false
    try {
      const ws = create()
      socket = ws
      const current = () => !disposed && socket === ws
      ws.onopen = () => {
        if (current()) status('connected')
      }
      ws.onmessage = (event) => {
        if (!current()) return
        received = true
        failures = 0
        message(event)
      }
      // Browsers emit error followed by close. Only close decides whether
      // to retry, avoiding a red flash and duplicate error notifications.
      ws.onerror = () => {}
      ws.onclose = (event) => {
        if (!current()) return
        if ((event.code === 1006 || !received) && failures < 3) {
          status('connecting')
          timer = schedule(connect, 500 * 2 ** failures++)
        } else if (event.code === 1000 && received) {
          status('complete')
        } else {
          status(
            'error',
            event.reason || `Log stream closed unexpectedly (${event.code})`
          )
        }
      }
    } catch (error) {
      status(
        'error',
        error instanceof Error ? error.message : 'Connection failed'
      )
    }
  }
  connect()
  return () => {
    disposed = true
    if (timer !== undefined) cancel(timer)
    if (socket) {
      socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null
      socket.close()
    }
  }
}
