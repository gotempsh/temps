export interface ProjectCardTrafficPoint {
  hour: string
  count: number
}

interface VisitorTrafficPoint {
  date: string
  count: number
}

interface ApiTrafficPoint {
  bucket: string
  request_count: number
}

export interface ProjectCardTraffic {
  data: ProjectCardTrafficPoint[]
  kind: 'visitors' | 'api-requests'
}

export function projectCardTraffic(
  visitors: VisitorTrafficPoint[] = [],
  apiRequests: ApiTrafficPoint[] = [],
  totalApiRequests = 0
): ProjectCardTraffic {
  const hasVisitorTraffic = visitors.some((point) => point.count > 0)

  if (hasVisitorTraffic || totalApiRequests === 0) {
    return {
      kind: 'visitors',
      data: visitors.map((point) => ({
        hour: point.date,
        count: point.count,
      })),
    }
  }

  return {
    kind: 'api-requests',
    data: apiRequests.map((point) => ({
      hour: point.bucket,
      count: point.request_count,
    })),
  }
}
