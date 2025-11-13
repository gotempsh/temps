import { ServiceTypeRoute } from '@/api/client/types.gen'

/**
 * Extract service type from Docker image name
 * Examples:
 * - "postgres:17-alpine" → "postgres"
 * - "mongo:latest" → "mongodb"
 * - "redis:7" → "redis"
 * - "mysql:8" → "mysql"
 * - "minio/minio:latest" → "s3"
 */
export function extractServiceTypeFromImage(image: string): ServiceTypeRoute | null {
  if (!image) return null

  const imageName = image.toLowerCase().split(':')[0].split('/').pop() || ''

  // Map common Docker image names to service types
  const serviceTypeMap: Record<string, ServiceTypeRoute> = {
    postgres: 'postgres',
    postgresql: 'postgres',
    mysql: 'mysql',
    mariadb: 'mysql',
    mongo: 'mongodb',
    mongodb: 'mongodb',
    redis: 'redis',
    minio: 's3',
    mongodb: 'mongodb',
  }

  return serviceTypeMap[imageName] || null
}

/**
 * Get service type with fallback to extracted type from image
 */
export function getServiceTypeWithFallback(
  providedType: ServiceTypeRoute | undefined,
  image: string | undefined
): ServiceTypeRoute | null {
  // If service type is provided, use it
  if (providedType) {
    return providedType
  }

  // Otherwise, try to extract from image name
  if (image) {
    return extractServiceTypeFromImage(image)
  }

  return null
}
