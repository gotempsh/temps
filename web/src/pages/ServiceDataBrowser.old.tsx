import {
  getContainerInfoOptions,
  getEntityInfoOptions,
  getServiceOptions,
  listContainersAtPathOptions,
  listEntitiesOptions,
  listRootContainersOptions,
  queryDataMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ContainerResponse,
  EntityInfoResponse,
  EntityResponse,
  FieldResponse,
  QueryDataRequest,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ChevronRight,
  Database,
  Folder,
  Home,
  Loader2,
  RefreshCcw,
  Table as TableIcon,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'

export function ServiceDataBrowser() {
  const { id } = useParams<{ id: string }>()
  const [searchParams, setSearchParams] = useSearchParams()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // Parse path from URL query params
  const pathParam = searchParams.get('path') || ''
  const entityParam = searchParams.get('entity') || ''
  const pathSegments = pathParam ? pathParam.split('/').filter(Boolean) : []

  // Pagination state
  const [page, setPage] = useState(1)
  const pageSize = 20

  // Get service details
  const {
    data: service,
    isLoading: serviceLoading,
    error: serviceError,
  } = useQuery({
    ...getServiceOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  // Get containers at current path
  const {
    data: containers,
    isLoading: containersLoading,
    error: containersError,
    refetch: refetchContainers,
  } = useQuery({
    ...(pathParam
      ? listContainersAtPathOptions({
          path: { service_id: parseInt(id!), path: pathParam },
        })
      : listRootContainersOptions({
          path: { service_id: parseInt(id!) },
        })),
    enabled: !!id && !entityParam,
  })

  // Get entities at current path (if path points to a container that can hold entities)
  const {
    data: entities,
    isLoading: entitiesLoading,
    error: entitiesError,
    refetch: refetchEntities,
  } = useQuery({
    ...listEntitiesOptions({
      path: { service_id: parseInt(id!), path: pathParam },
    }),
    enabled: !!id && !!pathParam && !entityParam,
  })

  // Get entity schema
  const {
    data: entityInfo,
    isLoading: entityInfoLoading,
    error: entityInfoError,
  } = useQuery({
    ...getEntityInfoOptions({
      path: {
        service_id: parseInt(id!),
        path: pathParam,
        entity: entityParam,
      },
    }),
    enabled: !!id && !!pathParam && !!entityParam,
  })

  // Query entity data
  const queryEntityData = useMutation({
    ...queryDataMutation(),
  })

  // Load entity data when entity is selected or page changes
  useEffect(() => {
    if (entityParam && pathParam && id) {
      const queryRequest: QueryDataRequest = {
        limit: pageSize,
        offset: (page - 1) * pageSize,
      }

      queryEntityData.mutate({
        path: {
          service_id: parseInt(id),
          path: pathParam,
          entity: entityParam,
        },
        body: queryRequest,
      })
    }
  // queryEntityData.mutate is stable and doesn't need to be in dependencies
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entityParam, pathParam, page, id, pageSize])

  // Update breadcrumbs
  useEffect(() => {
    const crumbs = [
      { label: 'Storage', href: '/storage' },
      {
        label: service?.service?.name || 'Service',
        href: `/storage/${id}`,
      },
      {
        label: 'Browse Data',
        href: `/storage/${id}/browse`,
      },
    ]

    // Add path segments to breadcrumbs
    pathSegments.forEach((segment, index) => {
      const segmentPath = pathSegments.slice(0, index + 1).join('/')
      crumbs.push({
        label: segment,
        href: `/storage/${id}/browse?path=${segmentPath}`,
      })
    })

    // Add entity to breadcrumbs
    if (entityParam) {
      crumbs.push({
        label: entityParam,
        href: `/storage/${id}/browse?path=${pathParam}&entity=${entityParam}`,
      })
    }

    setBreadcrumbs(crumbs)
  }, [setBreadcrumbs, id, service, pathParam, entityParam, pathSegments])

  usePageTitle(
    `${service?.service?.name || 'Service'} - ${entityParam || pathSegments[pathSegments.length - 1] || 'Browse Data'}`
  )

  // Navigate to container
  const navigateToContainer = (container: ContainerResponse) => {
    const newPath = pathParam
      ? `${pathParam}/${container.name}`
      : container.name
    setSearchParams({ path: newPath })
    setPage(1)
  }

  // Navigate to entity
  const navigateToEntity = (entity: EntityResponse) => {
    setSearchParams({ path: pathParam, entity: entity.name })
    setPage(1)
  }

  // Navigate back
  const navigateBack = () => {
    if (entityParam) {
      // Remove entity, stay in current path
      setSearchParams({ path: pathParam })
    } else if (pathSegments.length > 0) {
      // Go up one level in path
      const parentPath = pathSegments.slice(0, -1).join('/')
      if (parentPath) {
        setSearchParams({ path: parentPath })
      } else {
        setSearchParams({})
      }
    } else {
      // Go back to service detail
      navigate(`/storage/${id}`)
    }
    setPage(1)
  }

  // Loading state
  if (serviceLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6 space-y-6">
          <div className="h-8 w-32 bg-muted rounded animate-pulse" />
          <Card>
            <CardHeader>
              <div className="space-y-2">
                <div className="h-5 w-40 bg-muted rounded animate-pulse" />
                <div className="h-4 w-24 bg-muted rounded animate-pulse" />
              </div>
            </CardHeader>
          </Card>
        </div>
      </div>
    )
  }

  // Error state
  if (serviceError || !service) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6">
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Failed to load service details. Please try again.
            </AlertDescription>
          </Alert>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="p-6 space-y-6">
        {/* Header */}
        <div className="flex items-center gap-3">
          <Button variant="ghost" size="icon" onClick={navigateBack}>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <ServiceLogo
            service={service.service.service_type}
            className="h-8 w-8"
          />
          <div className="flex flex-col">
            <h1 className="text-2xl font-semibold">
              {service.service.name} - Data Browser
            </h1>
            <p className="text-sm text-muted-foreground">
              Explore containers and browse data for this service
            </p>
          </div>
        </div>

        {/* Path breadcrumbs */}
        <Card>
          <CardContent className="pt-6">
            <div className="flex items-center gap-2 flex-wrap">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setSearchParams({})
                  setPage(1)
                }}
                className="gap-2"
              >
                <Home className="h-4 w-4" />
                Root
              </Button>

              {pathSegments.map((segment, index) => (
                <div key={index} className="flex items-center gap-2">
                  <ChevronRight className="h-4 w-4 text-muted-foreground" />
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      const newPath = pathSegments.slice(0, index + 1).join('/')
                      setSearchParams({ path: newPath })
                      setPage(1)
                    }}
                  >
                    {segment}
                  </Button>
                </div>
              ))}

              {entityParam && (
                <div className="flex items-center gap-2">
                  <ChevronRight className="h-4 w-4 text-muted-foreground" />
                  <Badge variant="outline" className="gap-2">
                    <TableIcon className="h-3 w-3" />
                    {entityParam}
                  </Badge>
                </div>
              )}
            </div>
          </CardContent>
        </Card>

        {/* Content area */}
        {entityParam ? (
          // Show entity data and schema
          <EntityDataView
            entityInfo={entityInfo}
            entityInfoLoading={entityInfoLoading}
            entityInfoError={entityInfoError}
            queryResult={queryEntityData.data}
            queryLoading={queryEntityData.isPending}
            queryError={queryEntityData.error}
            page={page}
            pageSize={pageSize}
            onPageChange={setPage}
            onRefresh={() =>
              queryEntityData.mutate({
                path: {
                  service_id: parseInt(id!),
                  path: pathParam,
                  entity: entityParam,
                },
                body: {
                  limit: pageSize,
                  offset: (page - 1) * pageSize,
                },
              })
            }
          />
        ) : (
          // Show containers and entities
          <div className="grid gap-6 md:grid-cols-2">
            {/* Containers */}
            {(containers || containersLoading || containersError) && (
              <ContainerListView
                containers={containers}
                loading={containersLoading}
                error={containersError}
                onNavigate={navigateToContainer}
                onRefresh={refetchContainers}
              />
            )}

            {/* Entities */}
            {(entities || entitiesLoading || entitiesError) && (
              <EntityListView
                entities={entities}
                loading={entitiesLoading}
                error={entitiesError}
                onNavigate={navigateToEntity}
                onRefresh={refetchEntities}
              />
            )}
          </div>
        )}
      </div>
    </div>
  )
}

// Container List Component
function ContainerListView({
  containers,
  loading,
  error,
  onNavigate,
  onRefresh,
}: {
  containers?: ContainerResponse[]
  loading: boolean
  error: any
  onNavigate: (container: ContainerResponse) => void
  onRefresh: () => void
}) {
  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              <Database className="h-5 w-5" />
              Containers
            </CardTitle>
            <CardDescription>
              Databases, schemas, buckets, keyspaces, and collections
            </CardDescription>
          </div>
          {!loading && !error && (
            <Button
              variant="ghost"
              size="sm"
              onClick={onRefresh}
              className="gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
            </Button>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {loading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : error ? (
          <div className="text-center py-8">
            <AlertCircle className="h-8 w-8 mx-auto text-muted-foreground mb-2" />
            <p className="text-sm text-muted-foreground">
              Failed to load containers
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={onRefresh}
              className="mt-4 gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
              Try again
            </Button>
          </div>
        ) : containers && containers.length > 0 ? (
          <div className="space-y-2">
            {containers.map((container) => (
              <button
                key={container.name}
                onClick={() => onNavigate(container)}
                className="w-full text-left p-3 rounded-md border border-border hover:bg-accent transition-colors"
              >
                <div className="flex items-center gap-3">
                  <Folder className="h-5 w-5 text-muted-foreground flex-shrink-0" />
                  <div className="flex-1 min-w-0">
                    <p className="font-medium truncate">{container.name}</p>
                    <div className="flex items-center gap-2 flex-wrap mt-1">
                      <Badge variant="outline" className="text-xs">
                        {container.container_type}
                      </Badge>
                      {container.can_contain_entities && (
                        <Badge variant="secondary" className="text-xs">
                          {container.entity_type_label || 'entities'}
                        </Badge>
                      )}
                    </div>
                  </div>
                  <ChevronRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />
                </div>
              </button>
            ))}
          </div>
        ) : (
          <div className="text-center py-8 text-sm text-muted-foreground">
            No containers found
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// Entity List Component
function EntityListView({
  entities,
  loading,
  error,
  onNavigate,
  onRefresh,
}: {
  entities?: EntityResponse[]
  loading: boolean
  error: any
  onNavigate: (entity: EntityResponse) => void
  onRefresh: () => void
}) {
  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              <TableIcon className="h-5 w-5" />
              Entities
            </CardTitle>
            <CardDescription>
              Tables, views, collections, objects, and keys
            </CardDescription>
          </div>
          {!loading && !error && (
            <Button
              variant="ghost"
              size="sm"
              onClick={onRefresh}
              className="gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
            </Button>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {loading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : error ? (
          <div className="text-center py-8">
            <AlertCircle className="h-8 w-8 mx-auto text-muted-foreground mb-2" />
            <p className="text-sm text-muted-foreground">
              Failed to load entities
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={onRefresh}
              className="mt-4 gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
              Try again
            </Button>
          </div>
        ) : entities && entities.length > 0 ? (
          <div className="space-y-2">
            {entities.map((entity) => (
              <button
                key={entity.name}
                onClick={() => onNavigate(entity)}
                className="w-full text-left p-3 rounded-md border border-border hover:bg-accent transition-colors"
              >
                <div className="flex items-center gap-3">
                  <TableIcon className="h-5 w-5 text-muted-foreground flex-shrink-0" />
                  <div className="flex-1 min-w-0">
                    <p className="font-medium truncate">{entity.name}</p>
                    <div className="flex items-center gap-2 flex-wrap mt-1">
                      <Badge variant="outline" className="text-xs">
                        {entity.entity_type}
                      </Badge>
                      {entity.row_count !== null &&
                        entity.row_count !== undefined && (
                          <span className="text-xs text-muted-foreground">
                            ~{entity.row_count.toLocaleString()} rows
                          </span>
                        )}
                    </div>
                  </div>
                  <ChevronRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />
                </div>
              </button>
            ))}
          </div>
        ) : (
          <div className="text-center py-8 text-sm text-muted-foreground">
            No entities found
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// Entity Data View Component
function EntityDataView({
  entityInfo,
  entityInfoLoading,
  entityInfoError,
  queryResult,
  queryLoading,
  queryError,
  page,
  pageSize,
  onPageChange,
  onRefresh,
}: {
  entityInfo?: EntityInfoResponse
  entityInfoLoading: boolean
  entityInfoError: any
  queryResult?: any
  queryLoading: boolean
  queryError: any
  page: number
  pageSize: number
  onPageChange: (page: number) => void
  onRefresh: () => void
}) {
  const [showSchema, setShowSchema] = useState(false)

  if (entityInfoLoading || queryLoading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (entityInfoError || queryError) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertDescription>
          Failed to load entity data. Please try again.
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-6">
      {/* Entity Info Card */}
      {entityInfo && (
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle className="flex items-center gap-2">
                  <TableIcon className="h-5 w-5" />
                  {entityInfo.entity}
                </CardTitle>
                <CardDescription>
                  Type: {entityInfo.entity_type} • {entityInfo.fields?.length || 0}{' '}
                  fields
                </CardDescription>
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setShowSchema(!showSchema)}
                >
                  {showSchema ? 'Hide' : 'Show'} Schema
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={onRefresh}
                  className="gap-2"
                >
                  <RefreshCcw className="h-4 w-4" />
                  Refresh
                </Button>
              </div>
            </div>
          </CardHeader>
          {showSchema && entityInfo.fields && (
            <CardContent>
              <div className="space-y-2">
                <h3 className="font-medium text-sm mb-3">Schema</h3>
                <div className="rounded-md border">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b bg-muted/50">
                        <th className="text-left p-3 font-medium">Field</th>
                        <th className="text-left p-3 font-medium">Type</th>
                        <th className="text-left p-3 font-medium">Nullable</th>
                      </tr>
                    </thead>
                    <tbody>
                      {entityInfo.fields.map((field: FieldResponse) => (
                        <tr key={field.name} className="border-b last:border-0">
                          <td className="p-3 font-mono">{field.name}</td>
                          <td className="p-3">
                            <Badge variant="outline">{field.field_type}</Badge>
                          </td>
                          <td className="p-3">
                            <Badge
                              variant={field.nullable ? 'secondary' : 'default'}
                            >
                              {field.nullable ? 'Yes' : 'No'}
                            </Badge>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            </CardContent>
          )}
        </Card>
      )}

      {/* Data Table */}
      {queryResult && (
        <Card>
          <CardHeader>
            <CardTitle>Data</CardTitle>
            <CardDescription>
              Showing {queryResult.returned_count} of {queryResult.total_count || '?'}{' '}
              rows • Execution time: {queryResult.execution_time_ms}ms
            </CardDescription>
          </CardHeader>
          <CardContent>
            {queryResult.rows && queryResult.rows.length > 0 ? (
              <>
                <div className="rounded-md border overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b bg-muted/50">
                        {queryResult.fields?.map((field: FieldResponse) => (
                          <th
                            key={field.name}
                            className="text-left p-3 font-medium whitespace-nowrap"
                          >
                            {field.name}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {queryResult.rows.map((row: any, rowIndex: number) => (
                        <tr key={rowIndex} className="border-b last:border-0 hover:bg-muted/30">
                          {queryResult.fields?.map((field: FieldResponse) => (
                            <td
                              key={field.name}
                              className="p-3 font-mono text-xs max-w-xs truncate"
                              title={String(row[field.name])}
                            >
                              {row[field.name] !== null &&
                              row[field.name] !== undefined
                                ? String(row[field.name])
                                : '-'}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>

                {/* Pagination */}
                <div className="flex items-center justify-between mt-4">
                  <div className="text-sm text-muted-foreground">
                    Page {page} •  Rows {(page - 1) * pageSize + 1} -{' '}
                    {(page - 1) * pageSize + queryResult.returned_count}
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={page === 1}
                      onClick={() => onPageChange(page - 1)}
                    >
                      Previous
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={queryResult.returned_count < pageSize}
                      onClick={() => onPageChange(page + 1)}
                    >
                      Next
                    </Button>
                  </div>
                </div>
              </>
            ) : (
              <div className="text-center py-8 text-sm text-muted-foreground">
                No data found
              </div>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}
