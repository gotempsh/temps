export interface Span {
  id: string
  name: string
  serviceName: string
  operation: string
  startTimeUnixNano: number
  endTimeUnixNano: number
  error?: boolean
  children?: Span[]
}

export interface Trace {
  id: string
  name: string
  startTimeUnixNano: number
  endTimeUnixNano: number
  services: number
  depth: number
  totalSpans: number
  spans: Span[]
}
