import {
  checkExplorerSupportOptions,
  getEntityInfoOptions,
  getServiceOptions,
  listRootContainersOptions,
  queryDataMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { listContainersAtPath, listEntities } from '@/api/client/sdk.gen'
import type {
  ContainerResponse,
  EntityInfoResponse,
  EntityResponse,
  ExplorerSupportResponse,
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ScrollArea } from '@/components/ui/scroll-area'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ArrowUpDown,
  Box,
  ChevronDown,
  ChevronRight,
  Database,
  File,
  FileText,
  Folder,
  FolderOpen,
  Layers,
  Loader2,
  Package,
  RefreshCcw,
  Search,
  SortAsc,
  SortDesc,
  Table as TableIcon,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'

interface TreeNode {
  name: string
  path: string
  type: 'container' | 'entity'
  isExpanded?: boolean
  isLoaded?: boolean
  children?: TreeNode[]
  containerType?: string
  entityType?: string
  level?: number // Hierarchy level (0 = root, 1 = first level, etc.)
  canContainContainers?: boolean
  canContainEntities?: boolean
}

export function ServiceDataBrowser() {
  const { id } = useParams<{ id: string }>()
  const [searchParams, setSearchParams] = useSearchParams()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // Parse path and entity from URL
  const pathParam = searchParams.get('path') || ''
  const entityParam = searchParams.get('entity') || ''

  // Tree state
  const [treeNodes, setTreeNodes] = useState<TreeNode[]>([])
  const [selectedPath, setSelectedPath] = useState<string>(pathParam)
  const [selectedEntity, setSelectedEntity] = useState<string>(entityParam)
  const [treeError, setTreeError] = useState<string | null>(null)

  // Filter state (for sidebar tree only)
  const [filterText, setFilterText] = useState('')

  // Pagination state
  const [page, setPage] = useState(1)
  const pageSize = 20

  // Data table filter and sort state
  const [dataFilter, setDataFilter] = useState<unknown>(undefined)
  const [dataFilterInput, setDataFilterInput] = useState('') // Local input state before apply
  const [filterFormData, setFilterFormData] = useState<Record<string, any>>({}) // For schema-based filters
  const [dataSortField, setDataSortField] = useState<string>('')
  const [dataSortOrder, setDataSortOrder] = useState<'asc' | 'desc'>('asc')

  // Apply filter handler
  const handleApplyFilter = () => {
    // If we have filter_schema, send the form data as JSON object
    if (explorerSupport?.filter_schema) {
      setDataFilter(filterFormData)
    } else {
      // For SQL capability, send as text (or wrap in object if backend expects it)
      setDataFilter(dataFilterInput || undefined)
    }
    setPage(1) // Reset to first page when filter changes
  }

  // Clear filter handler
  const handleClearFilter = () => {
    setDataFilterInput('')
    setDataFilter(undefined)
    setFilterFormData({})
    setPage(1)
  }

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

  // Get explorer support capabilities
  const { data: explorerSupport, isLoading: explorerSupportLoading } = useQuery(
    {
      ...checkExplorerSupportOptions({
        path: { service_id: parseInt(id!) },
      }),
      enabled: !!id,
    }
  )

  // Helper function to get hierarchy capabilities for a given level
  const getHierarchyCapabilities = (level: number) => {
    if (!explorerSupport?.hierarchy || explorerSupport.hierarchy.length === 0) {
      // Fallback: if no hierarchy, assume containers can contain both
      return {
        can_list_containers: true,
        can_list_entities: true,
        container_type: 'folder',
      }
    }

    // Find the hierarchy level configuration
    const hierarchyLevel = explorerSupport.hierarchy.find((h) => h.level === level)
    if (!hierarchyLevel) {
      // If level not found, use the last level configuration
      const lastLevel = explorerSupport.hierarchy[explorerSupport.hierarchy.length - 1]
      return {
        can_list_containers: lastLevel.can_list_containers,
        can_list_entities: lastLevel.can_list_entities,
        container_type: lastLevel.container_type,
      }
    }

    return {
      can_list_containers: hierarchyLevel.can_list_containers,
      can_list_entities: hierarchyLevel.can_list_entities,
      container_type: hierarchyLevel.container_type,
    }
  }

  // Helper function to get appropriate icon for container
  const getContainerIcon = (
    containerType: string | undefined,
    isExpanded: boolean
  ) => {
    const type = containerType?.toLowerCase() || 'folder'
    const className = 'h-4 w-4 text-muted-foreground flex-shrink-0'

    switch (type) {
      case 'bucket':
        return <Package className={className} />
      case 'schema':
        return <Database className={className} />
      case 'database':
        return <Database className={className} />
      case 'namespace':
        return <Layers className={className} />
      case 'object':
      case 'folder':
      default:
        return isExpanded ? (
          <FolderOpen className={className} />
        ) : (
          <Folder className={className} />
        )
    }
  }

  // Helper function to get appropriate icon for entity
  const getEntityIcon = (entityType: string | undefined) => {
    const type = entityType?.toLowerCase() || 'table'
    const className = 'h-4 w-4 text-muted-foreground flex-shrink-0'

    switch (type) {
      case 'object':
        return <File className={className} />
      case 'table':
        return <TableIcon className={className} />
      case 'view':
        return <FileText className={className} />
      case 'collection':
        return <Box className={className} />
      default:
        return <TableIcon className={className} />
    }
  }

  // Helper function to determine if we're dealing with an object store
  const isObjectStore = () => {
    return explorerSupport?.capabilities?.includes('object-store') || false
  }

  // Get root containers
  const {
    data: rootContainers,
    isLoading: rootLoading,
    error: rootContainersError,
    refetch: refetchRoot,
  } = useQuery({
    ...listRootContainersOptions({
      path: { service_id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  // Initialize tree with root containers
  useEffect(() => {
    if (rootContainers && treeNodes.length === 0) {
      // Root containers are always level 0
      const rootLevel = 0
      const hierarchyInfo = getHierarchyCapabilities(rootLevel)

      const nodes: TreeNode[] = rootContainers.map((container) => ({
        name: container.name,
        path: container.name,
        type: 'container' as const,
        isExpanded: false,
        isLoaded: false,
        children: [],
        level: rootLevel,
        containerType: container.container_type || hierarchyInfo.container_type,
        canContainContainers: hierarchyInfo.can_list_containers,
        canContainEntities: hierarchyInfo.can_list_entities,
      }))
      setTreeNodes(nodes)
    }
  }, [rootContainers, treeNodes.length, explorerSupport])

  // Get entity info when entity is selected
  const { data: entityInfo, isLoading: entityInfoLoading } = useQuery({
    ...getEntityInfoOptions({
      path: {
        service_id: parseInt(id!),
        path: selectedPath,
        entity: selectedEntity,
      },
    }),
    enabled: !!id && !!selectedPath && !!selectedEntity,
  })

  // Query entity data
  const queryEntityData = useMutation({
    ...queryDataMutation(),
  })

  // Load entity data when entity is selected or page changes
  useEffect(() => {
    if (selectedEntity && selectedPath && id) {
      const queryRequest: QueryDataRequest = {
        limit: pageSize,
        offset: (page - 1) * pageSize,
        sort_by: dataSortField || undefined,
        sort_order: dataSortField ? dataSortOrder : undefined,
        filters: dataFilter || undefined,
      }

      queryEntityData.mutate({
        path: {
          service_id: parseInt(id),
          path: selectedPath,
          entity: selectedEntity,
        },
        body: queryRequest,
      })
    }
    // queryEntityData.mutate is stable and doesn't need to be in dependencies
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    selectedEntity,
    selectedPath,
    page,
    id,
    pageSize,
    dataSortField,
    dataSortOrder,
    dataFilter,
  ])

  // Update breadcrumbs
  useEffect(() => {
    const crumbs = [
      { label: 'Storage', href: '/storage' },
      {
        label: service?.service?.name || 'Service',
        href: `/storage/${id}`,
      },
      { label: 'Browse Data', href: `/storage/${id}/browse` },
    ]

    if (selectedPath) {
      crumbs.push({ label: selectedPath, href: '' })
    }
    if (selectedEntity) {
      crumbs.push({ label: selectedEntity, href: '' })
    }

    setBreadcrumbs(crumbs)
  }, [setBreadcrumbs, id, service, selectedPath, selectedEntity])

  usePageTitle(
    `${service?.service?.name || 'Service'} - ${selectedEntity || selectedPath || 'Browse Data'}`
  )

  // Toggle tree node expansion
  const toggleNode = async (nodePath: string) => {
    // Find node BEFORE toggling to check its current state
    const findNode = (nodes: TreeNode[], path: string): TreeNode | null => {
      for (const node of nodes) {
        if (node.path === path) return node
        if (node.children) {
          const found = findNode(node.children, path)
          if (found) return found
        }
      }
      return null
    }

    const node = findNode(treeNodes, nodePath)
    const wasExpanded = node?.isExpanded || false
    const needsLoading = node && !node.isLoaded && !wasExpanded

    // Update tree nodes - toggle expansion
    const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
      return nodes.map((node) => {
        if (node.path === nodePath) {
          // Toggle this node
          return {
            ...node,
            isExpanded: !node.isExpanded,
          }
        } else if (nodePath.startsWith(node.path + '/')) {
          // Recursively update children
          return {
            ...node,
            children: node.children ? updateNodes(node.children) : [],
          }
        }
        return node
      })
    }

    setTreeNodes(updateNodes(treeNodes))

    // Load children if expanding for the first time
    if (needsLoading) {
      await loadNodeChildren(nodePath)
    }
  }

  // Load children for a node
  const loadNodeChildren = async (nodePath: string) => {
    try {
      setTreeError(null) // Clear any previous errors
      let containersData: ContainerResponse[] = []
      let entitiesData: EntityResponse[] = []
      let hasError = false
      let errorMessage = ''

      // Try to fetch containers at this path (may not exist for all service types)
      try {
        const containersResponse = await listContainersAtPath({
          path: { service_id: parseInt(id!), path: nodePath },
        })
        if (containersResponse.data && Array.isArray(containersResponse.data)) {
          containersData = containersResponse.data
        }
      } catch (error: any) {
        // Check if this is a real error or just no containers
        if (error?.detail) {
          hasError = true
          errorMessage = error.detail
        } else {
          console.debug('No containers at path:', nodePath)
        }
      }

      // Try to fetch entities at this path (may not exist for all service types)
      try {
        const entitiesResponse = await listEntities({
          path: { service_id: parseInt(id!), path: nodePath },
        })
        if (entitiesResponse.data && Array.isArray(entitiesResponse.data)) {
          entitiesData = entitiesResponse.data
        }
      } catch (error: any) {
        // Check if this is a real error or just no entities
        if (error?.detail) {
          hasError = true
          errorMessage = error.detail
        } else {
          console.debug('No entities at path:', nodePath)
        }
      }

      // If we got a real error, show it
      if (hasError) {
        setTreeError(errorMessage)
        return
      }

      const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
        return nodes.map((node) => {
          if (node.path === nodePath) {
            const children: TreeNode[] = []
            // Calculate child level (current level + 1)
            const currentLevel = node.level !== undefined ? node.level : 0
            const childLevel = currentLevel + 1
            const childHierarchyInfo = getHierarchyCapabilities(childLevel)

            // Add containers
            containersData.forEach((container: ContainerResponse) => {
              children.push({
                name: container.name,
                path: `${nodePath}/${container.name}`,
                type: 'container',
                isExpanded: false,
                isLoaded: false,
                children: [],
                level: childLevel,
                containerType:
                  container.container_type || childHierarchyInfo.container_type,
                canContainContainers: childHierarchyInfo.can_list_containers,
                canContainEntities: childHierarchyInfo.can_list_entities,
              })
            })

            // Add entities
            entitiesData.forEach((entity: EntityResponse) => {
              children.push({
                name: entity.name,
                path: `${nodePath}/${entity.name}`,
                type: 'entity',
                level: childLevel,
                entityType: entity.entity_type,
              })
            })

            return {
              ...node,
              isLoaded: true,
              children,
            }
          } else if (node.children) {
            return {
              ...node,
              children: updateNodes(node.children),
            }
          }
          return node
        })
      }

      setTreeNodes(updateNodes(treeNodes))
    } catch (error: any) {
      console.error('Failed to load node children:', error)
      setTreeError(error?.detail || 'Failed to load containers and entities')
    }
  }

  // Handle node click
  const handleNodeClick = async (node: TreeNode) => {
    if (node.type === 'container') {
      setSelectedPath(node.path)
      setSelectedEntity('')
      setSearchParams({ path: node.path })
      setPage(1)

      // Find the current node state
      const findNode = (nodes: TreeNode[], path: string): TreeNode | null => {
        for (const n of nodes) {
          if (n.path === path) return n
          if (n.children) {
            const found = findNode(n.children, path)
            if (found) return found
          }
        }
        return null
      }

      const currentNode = findNode(treeNodes, node.path)
      const canHaveChildren =
        node.canContainContainers || node.canContainEntities

      // Only toggle if the node can have children
      if (canHaveChildren) {
        const isCurrentlyExpanded = currentNode?.isExpanded || false
        const needsLoading =
          currentNode && !currentNode.isLoaded && !isCurrentlyExpanded

        // Toggle expansion state
        const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
          return nodes.map((n) => {
            if (n.path === node.path) {
              return { ...n, isExpanded: !isCurrentlyExpanded }
            } else if (node.path.startsWith(n.path + '/')) {
              return {
                ...n,
                children: n.children ? updateNodes(n.children) : [],
              }
            }
            return n
          })
        }

        setTreeNodes(updateNodes(treeNodes))

        // Load children if expanding for the first time
        if (needsLoading) {
          await loadNodeChildren(node.path)
        }
      }
    } else if (node.type === 'entity') {
      setSelectedEntity(node.name)
      setSelectedPath(node.path.split('/').slice(0, -1).join('/'))
      setSearchParams({
        path: node.path.split('/').slice(0, -1).join('/'),
        entity: node.name,
      })
      setPage(1)
    }
  }

  // Filter nodes recursively - shows full tree path to matches
  const filterNodes = (nodes: TreeNode[], searchText: string): TreeNode[] => {
    if (!searchText.trim()) return nodes

    const filtered: TreeNode[] = []
    const lowerSearch = searchText.toLowerCase()

    // Helper function to check if node or any descendant matches
    const hasMatchInTree = (node: TreeNode): boolean => {
      const matchesName = node.name.toLowerCase().includes(lowerSearch)
      const matchesType =
        node.containerType?.toLowerCase().includes(lowerSearch) ||
        node.entityType?.toLowerCase().includes(lowerSearch)

      if (matchesName || matchesType) return true

      if (node.children) {
        return node.children.some((child) => hasMatchInTree(child))
      }

      return false
    }

    for (const node of nodes) {
      // Check if this node or any descendant matches
      if (hasMatchInTree(node)) {
        // Filter children recursively
        const filteredChildren = node.children
          ? filterNodes(node.children, searchText)
          : []

        // Include this node (even if it doesn't match) if it has matching descendants
        // This preserves the full path to matching items
        filtered.push({
          ...node,
          children: filteredChildren,
          // Auto-expand if it has matching children to show the full tree
          isExpanded: filteredChildren.length > 0 ? true : node.isExpanded,
        })
      }
    }

    return filtered
  }

  // Get filtered nodes
  const getProcessedNodes = (): TreeNode[] => {
    if (filterText) {
      return filterNodes(treeNodes, filterText)
    }
    return treeNodes
  }

  // Loading state
  if (serviceLoading || rootLoading || explorerSupportLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6">
          <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      </div>
    )
  }

  // Error state - Service load error
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

  // Error state - Root containers error
  if (rootContainersError) {
    const error = rootContainersError as any
    const errorTitle = error?.title || 'Connection Error'
    const errorDetail =
      error?.detail ||
      'Failed to connect to the service. Please check the service status and try again.'

    return (
      <div className="flex-1 overflow-hidden flex flex-col">
        {/* Header */}
        <div className="p-6 pb-0">
          <div className="flex items-center gap-3 mb-4">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => navigate(`/storage/${id}`)}
            >
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
                Explore containers and browse data
              </p>
            </div>
          </div>
        </div>

        {/* Error state */}
        <div className="flex-1 flex items-center justify-center p-6">
          <Card className="max-w-2xl w-full">
            <CardHeader className="text-center">
              <div className="mx-auto mb-4 h-16 w-16 rounded-full bg-destructive/10 flex items-center justify-center">
                <AlertCircle className="h-8 w-8 text-destructive" />
              </div>
              <CardTitle className="text-xl text-destructive">
                {errorTitle}
              </CardTitle>
              <CardDescription className="text-base mt-2">
                {errorDetail}
              </CardDescription>
            </CardHeader>
            <CardContent className="text-center space-y-3">
              <div className="flex gap-2 justify-center">
                <Button
                  onClick={() => refetchRoot()}
                  variant="default"
                  className="gap-2"
                >
                  <RefreshCcw className="h-4 w-4" />
                  Retry
                </Button>
                <Button
                  onClick={() => navigate(`/storage/${id}`)}
                  variant="outline"
                  className="gap-2"
                >
                  <ArrowLeft className="h-4 w-4" />
                  Back to Service
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  // Check if explorer is supported
  if (explorerSupport && !explorerSupport.supported) {
    return (
      <div className="flex-1 overflow-hidden flex flex-col">
        {/* Header */}
        <div className="p-6 pb-0">
          <div className="flex items-center gap-3 mb-4">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => navigate(`/storage/${id}`)}
            >
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
                Explore containers and browse data
              </p>
            </div>
          </div>
        </div>

        {/* Empty state */}
        <div className="flex-1 flex items-center justify-center p-6">
          <Card className="max-w-2xl w-full">
            <CardHeader className="text-center">
              <div className="mx-auto mb-4 h-16 w-16 rounded-full bg-muted flex items-center justify-center">
                <Database className="h-8 w-8 text-muted-foreground" />
              </div>
              <CardTitle className="text-xl">
                Data Explorer Not Available
              </CardTitle>
              <CardDescription className="text-base mt-2">
                The data explorer is not supported for{' '}
                <span className="font-semibold">
                  {explorerSupport.service_type}
                </span>{' '}
                services.
              </CardDescription>
            </CardHeader>
            {explorerSupport.reason && (
              <CardContent className="text-center">
                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>{explorerSupport.reason}</AlertDescription>
                </Alert>
              </CardContent>
            )}
            <CardContent className="text-center pt-0">
              <Button
                onClick={() => navigate(`/storage/${id}`)}
                variant="outline"
                className="gap-2"
              >
                <ArrowLeft className="h-4 w-4" />
                Back to Service
              </Button>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-hidden flex flex-col">
      {/* Header */}
      <div className="p-6 pb-0">
        <div className="flex items-center gap-3 mb-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/storage/${id}`)}
          >
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
              Explore containers and browse data
            </p>
          </div>
        </div>
      </div>

      {/* Main content area with sidebar */}
      <div className="flex-1 flex overflow-hidden px-6 pb-6">
        {/* Sidebar - Tree View */}
        <div className="w-80 border-r pr-4">
          <Card className="h-full flex flex-col">
            <CardHeader className="pb-3">
              <CardTitle className="text-base flex items-center gap-2">
                <Database className="h-4 w-4" />
                Containers
              </CardTitle>
              <CardDescription className="text-xs">
                Navigate through your data
              </CardDescription>
            </CardHeader>

            {/* Search Control */}
            <div className="px-4 pb-3">
              <div className="relative">
                <Search className="absolute left-2 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                <input
                  type="text"
                  placeholder="Filter..."
                  value={filterText}
                  onChange={(e) => setFilterText(e.target.value)}
                  className="w-full pl-8 pr-8 py-1.5 text-sm border rounded-md bg-background focus:outline-none focus:ring-2 focus:ring-ring"
                />
                {filterText && (
                  <button
                    onClick={() => setFilterText('')}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                  >
                    <X className="h-4 w-4" />
                  </button>
                )}
              </div>
            </div>

            <CardContent className="flex-1 p-0 overflow-hidden border-t">
              {/* Show tree error if present */}
              {treeError && (
                <div className="p-4 border-b">
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription className="text-xs">
                      {treeError}
                    </AlertDescription>
                  </Alert>
                  <Button
                    onClick={() => {
                      setTreeError(null)
                      refetchRoot()
                    }}
                    variant="outline"
                    size="sm"
                    className="w-full mt-2 gap-2"
                  >
                    <RefreshCcw className="h-3 w-3" />
                    Retry
                  </Button>
                </div>
              )}
              <ScrollArea className="h-full px-4 pb-4 pt-2">
                {getProcessedNodes().length > 0 ? (
                  <TreeView
                    nodes={getProcessedNodes()}
                    onToggle={toggleNode}
                    onNodeClick={handleNodeClick}
                    selectedPath={selectedPath}
                    selectedEntity={selectedEntity}
                    getContainerIcon={getContainerIcon}
                    getEntityIcon={getEntityIcon}
                  />
                ) : (
                  <div className="text-center py-8 text-sm text-muted-foreground">
                    {filterText ? 'No matches found' : 'No containers'}
                  </div>
                )}
              </ScrollArea>
            </CardContent>
          </Card>
        </div>

        {/* Main content */}
        <div className="flex-1 pl-6 overflow-auto">
          {selectedEntity ? (
            // Show entity data
            <EntityDataView
              entityInfo={entityInfo}
              entityInfoLoading={entityInfoLoading}
              queryResult={queryEntityData.data}
              queryLoading={queryEntityData.isPending}
              queryError={queryEntityData.error}
              page={page}
              pageSize={pageSize}
              onPageChange={setPage}
              dataFilterInput={dataFilterInput}
              onDataFilterInputChange={setDataFilterInput}
              filterFormData={filterFormData}
              onFilterFormDataChange={setFilterFormData}
              appliedFilter={dataFilter}
              onApplyFilter={handleApplyFilter}
              onClearFilter={handleClearFilter}
              dataSortField={dataSortField}
              dataSortOrder={dataSortOrder}
              explorerSupport={explorerSupport}
              onSort={(field: string) => {
                if (dataSortField === field) {
                  // Toggle sort order if same field
                  setDataSortOrder(dataSortOrder === 'asc' ? 'desc' : 'asc')
                } else {
                  // New field, default to ascending
                  setDataSortField(field)
                  setDataSortOrder('asc')
                }
                setPage(1) // Reset to first page when sorting
              }}
              onRefresh={() => {
                if (selectedEntity && selectedPath && id) {
                  queryEntityData.mutate({
                    path: {
                      service_id: parseInt(id),
                      path: selectedPath,
                      entity: selectedEntity,
                    },
                    body: {
                      limit: pageSize,
                      offset: (page - 1) * pageSize,
                      sort_by: dataSortField || undefined,
                      sort_order: dataSortField ? dataSortOrder : undefined,
                      filters: dataFilter || undefined,
                    },
                  })
                }
              }}
              getEntityIcon={getEntityIcon}
              isObjectStore={isObjectStore}
            />
          ) : selectedPath ? (
            // Show container info
            <Card>
              <CardHeader>
                <CardTitle>
                  Container: {selectedPath.split('/').pop()}
                </CardTitle>
                <CardDescription>
                  Select an entity from the sidebar to view its data
                </CardDescription>
              </CardHeader>
              <CardContent>
                <p className="text-sm text-muted-foreground">
                  Expand folders in the sidebar to navigate through your data
                  structure.
                </p>
              </CardContent>
            </Card>
          ) : (
            // Show welcome message
            <Card>
              <CardHeader>
                <CardTitle>Welcome to Data Browser</CardTitle>
                <CardDescription>
                  Select a container from the sidebar to get started
                </CardDescription>
              </CardHeader>
              <CardContent>
                <p className="text-sm text-muted-foreground">
                  Use the tree navigation on the left to browse through
                  containers, schemas, and tables.
                </p>
              </CardContent>
            </Card>
          )}
        </div>
      </div>
    </div>
  )
}

// Tree View Component
function TreeView({
  nodes,
  level = 0,
  onToggle,
  onNodeClick,
  selectedPath,
  selectedEntity,
  getContainerIcon,
  getEntityIcon,
}: {
  nodes: TreeNode[]
  level?: number
  onToggle: (path: string) => void
  onNodeClick: (node: TreeNode) => void
  selectedPath: string
  selectedEntity: string
  getContainerIcon: (containerType: string | undefined, isExpanded: boolean) => JSX.Element
  getEntityIcon: (entityType: string | undefined) => JSX.Element
}) {
  return (
    <div className="space-y-1">
      {nodes.map((node) => (
        <TreeNodeComponent
          key={node.path}
          node={node}
          level={level}
          onToggle={onToggle}
          onNodeClick={onNodeClick}
          selectedPath={selectedPath}
          selectedEntity={selectedEntity}
          getContainerIcon={getContainerIcon}
          getEntityIcon={getEntityIcon}
        />
      ))}
    </div>
  )
}

// Tree Node Component
function TreeNodeComponent({
  node,
  level,
  onToggle,
  onNodeClick,
  selectedPath,
  selectedEntity,
  getContainerIcon,
  getEntityIcon,
}: {
  node: TreeNode
  level: number
  onToggle: (path: string) => void
  onNodeClick: (node: TreeNode) => void
  selectedPath: string
  selectedEntity: string
  getContainerIcon: (containerType: string | undefined, isExpanded: boolean) => JSX.Element
  getEntityIcon: (entityType: string | undefined) => JSX.Element
}) {
  const isSelected =
    node.type === 'container'
      ? node.path === selectedPath && !selectedEntity
      : node.path === `${selectedPath}/${selectedEntity}`

  const hasChildren = node.canContainContainers || node.canContainEntities

  return (
    <div>
      <button
        onClick={() => {
          // Only call onNodeClick - it handles the toggle internally
          onNodeClick(node)
        }}
        className={`w-full flex items-center gap-2 px-2 py-1.5 text-sm rounded-md transition-colors hover:bg-accent ${
          isSelected ? 'bg-accent text-accent-foreground' : ''
        }`}
        style={{ paddingLeft: `${level * 16 + 8}px` }}
      >
        {node.type === 'container' && hasChildren && (
          <span className="flex-shrink-0">
            {node.isExpanded ? (
              <ChevronDown className="h-3.5 w-3.5" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5" />
            )}
          </span>
        )}
        {node.type === 'container'
          ? getContainerIcon(node.containerType, node.isExpanded || false)
          : getEntityIcon(node.entityType)}
        <span className="truncate flex-1 text-left">{node.name}</span>
        {node.containerType && (
          <Badge variant="outline" className="text-xs flex-shrink-0">
            {node.containerType}
          </Badge>
        )}
      </button>
      {node.isExpanded && node.children && node.children.length > 0 && (
        <TreeView
          nodes={node.children}
          level={level + 1}
          onToggle={onToggle}
          onNodeClick={onNodeClick}
          selectedPath={selectedPath}
          selectedEntity={selectedEntity}
          getContainerIcon={getContainerIcon}
          getEntityIcon={getEntityIcon}
        />
      )}
    </div>
  )
}

// Dynamic Filter Builder Component
function DynamicFilterBuilder({
  schema,
  formData,
  onFormDataChange,
  onApplyFilter,
}: {
  schema: any
  formData: Record<string, any>
  onFormDataChange: (data: Record<string, any>) => void
  onApplyFilter?: () => void
}) {
  if (!schema || !schema.properties) {
    return null
  }

  const handleFieldChange = (fieldName: string, value: any) => {
    onFormDataChange({
      ...formData,
      [fieldName]: value,
    })
  }

  const renderField = (fieldName: string, fieldSchema: any) => {
    const value = formData[fieldName] || ''
    const type = fieldSchema.type
    const title = fieldSchema.title || fieldName
    const description = fieldSchema.description
    const uiWidget = fieldSchema['x-ui-widget'] // UI widget type
    const uiPlaceholder = fieldSchema['x-ui-placeholder'] // Custom placeholder
    const uiRows = fieldSchema['x-ui-rows'] || 3 // Textarea rows
    const examples = fieldSchema.examples || []

    // Enum/Select field
    if (fieldSchema.enum) {
      return (
        <div key={fieldName} className="space-y-2">
          <Label htmlFor={fieldName}>{title}</Label>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          <Select
            value={value}
            onValueChange={(val) => handleFieldChange(fieldName, val)}
          >
            <SelectTrigger>
              <SelectValue
                placeholder={uiPlaceholder || `Select ${title.toLowerCase()}`}
              />
            </SelectTrigger>
            <SelectContent>
              {fieldSchema.enum.map((option: any) => (
                <SelectItem key={option} value={String(option)}>
                  {String(option)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )
    }

    // Textarea widget or long text
    if (uiWidget === 'textarea' || fieldSchema.maxLength > 200) {
      return (
        <div key={fieldName} className="space-y-2">
          <Label htmlFor={fieldName}>{title}</Label>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          {examples.length > 0 && (
            <details className="text-xs text-muted-foreground">
              <summary className="cursor-pointer hover:text-foreground">
                Show examples
              </summary>
              <ul className="mt-1 ml-4 list-disc space-y-1">
                {examples.map((ex: string, i: number) => (
                  <li key={i} className="font-mono">
                    {ex}
                  </li>
                ))}
              </ul>
            </details>
          )}
          <Textarea
            id={fieldName}
            value={value}
            onChange={(e) => handleFieldChange(fieldName, e.target.value)}
            onKeyDown={(e) => {
              // Apply filter on Ctrl+Enter or Cmd+Enter
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                e.preventDefault()
                if (onApplyFilter) {
                  onApplyFilter()
                }
              }
            }}
            placeholder={uiPlaceholder || `Enter ${title.toLowerCase()}`}
            rows={uiRows}
            className="font-mono text-sm"
          />
        </div>
      )
    }

    // Number input
    if (type === 'number' || type === 'integer') {
      return (
        <div key={fieldName} className="space-y-2">
          <Label htmlFor={fieldName}>{title}</Label>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          <Input
            id={fieldName}
            type="number"
            value={value}
            onChange={(e) =>
              handleFieldChange(
                fieldName,
                type === 'integer'
                  ? parseInt(e.target.value) || 0
                  : parseFloat(e.target.value) || 0
              )
            }
            placeholder={uiPlaceholder || `Enter ${title.toLowerCase()}`}
            min={fieldSchema.minimum}
            max={fieldSchema.maximum}
          />
        </div>
      )
    }

    // Boolean/checkbox
    if (type === 'boolean') {
      return (
        <div key={fieldName} className="flex items-center space-x-2">
          <input
            id={fieldName}
            type="checkbox"
            checked={value || false}
            onChange={(e) => handleFieldChange(fieldName, e.target.checked)}
            className="h-4 w-4 rounded border-input"
          />
          <Label htmlFor={fieldName} className="font-normal">
            {title}
            {description && (
              <span className="text-xs text-muted-foreground ml-2">
                ({description})
              </span>
            )}
          </Label>
        </div>
      )
    }

    // Default: String input
    return (
      <div key={fieldName} className="space-y-2">
        <Label htmlFor={fieldName}>{title}</Label>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
        <Input
          id={fieldName}
          type="text"
          value={value}
          onChange={(e) => handleFieldChange(fieldName, e.target.value)}
          placeholder={uiPlaceholder || `Enter ${title.toLowerCase()}`}
          maxLength={fieldSchema.maxLength}
        />
      </div>
    )
  }

  return (
    <div className="space-y-4">
      {Object.entries(schema.properties).map(
        ([fieldName, fieldSchema]: [string, any]) =>
          renderField(fieldName, fieldSchema)
      )}
    </div>
  )
}

// Entity Data View Component
function EntityDataView({
  entityInfo,
  entityInfoLoading,
  queryResult,
  queryLoading,
  queryError,
  page,
  pageSize,
  onPageChange,
  dataFilterInput,
  onDataFilterInputChange,
  filterFormData,
  onFilterFormDataChange,
  appliedFilter,
  onApplyFilter,
  onClearFilter,
  dataSortField,
  dataSortOrder,
  explorerSupport,
  onSort,
  onRefresh,
  getEntityIcon,
  isObjectStore,
}: {
  entityInfo?: EntityInfoResponse
  entityInfoLoading: boolean
  queryResult?: any
  queryLoading: boolean
  queryError: any
  page: number
  pageSize: number
  onPageChange: (page: number) => void
  dataFilterInput: string
  onDataFilterInputChange: (filter: string) => void
  filterFormData: Record<string, any>
  onFilterFormDataChange: (data: Record<string, any>) => void
  appliedFilter: unknown
  onApplyFilter: () => void
  onClearFilter: () => void
  dataSortField: string
  dataSortOrder: 'asc' | 'desc'
  explorerSupport?: ExplorerSupportResponse
  onSort: (field: string) => void
  onRefresh: () => void
  getEntityIcon: (entityType: string | undefined) => JSX.Element
  isObjectStore: () => boolean
}) {
  const [showSchema, setShowSchema] = useState(false)

  // Check if SQL capability is available (for filter support)
  const hasSqlCapability =
    explorerSupport?.capabilities.includes('sql') || false
  const hasFilterSchema = explorerSupport?.filter_schema !== undefined
  const hasFilterSupport = hasFilterSchema || hasSqlCapability

  // Show skeleton loading while data is being fetched
  if (entityInfoLoading || queryLoading) {
    return (
      <div className="space-y-6">
        {/* Entity Info Card Skeleton */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="space-y-2 flex-1">
                <Skeleton className="h-6 w-48" />
                <Skeleton className="h-4 w-64" />
              </div>
              <div className="flex items-center gap-2">
                <Skeleton className="h-9 w-32" />
                <Skeleton className="h-9 w-24" />
              </div>
            </div>
          </CardHeader>
        </Card>

        {/* Data Table Card Skeleton */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="space-y-2">
                <Skeleton className="h-6 w-32" />
                <Skeleton className="h-4 w-96" />
              </div>
            </div>
            {/* Filter skeleton */}
            <div className="mt-4 space-y-3">
              <Skeleton className="h-10 w-full" />
              <div className="flex gap-2">
                <Skeleton className="h-10 w-32" />
                <Skeleton className="h-10 w-24" />
              </div>
            </div>
          </CardHeader>
          <CardContent>
            {/* Table skeleton */}
            <div className="space-y-3">
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
            </div>
            {/* Pagination skeleton */}
            <div className="flex items-center justify-between mt-4">
              <Skeleton className="h-4 w-48" />
              <div className="flex items-center gap-2">
                <Skeleton className="h-9 w-24" />
                <Skeleton className="h-9 w-24" />
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  // Extract error if present (but don't block rendering)
  const error = queryError as any
  const errorTitle = error?.title
  const errorDetail = error?.detail

  return (
    <div className="space-y-6">
      {/* Entity Info Card */}
      {entityInfo && (
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle className="flex items-center gap-2">
                  <div className="[&>svg]:h-5 [&>svg]:w-5">
                    {getEntityIcon(entityInfo.entity_type)}
                  </div>
                  {entityInfo.entity}
                </CardTitle>
                <CardDescription>
                  Type: {entityInfo.entity_type}
                  {!isObjectStore() && entityInfo.fields && (
                    <> • {entityInfo.fields.length} fields</>
                  )}
                </CardDescription>
              </div>
              <div className="flex items-center gap-2">
                {!isObjectStore() && entityInfo.fields && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setShowSchema(!showSchema)}
                  >
                    {showSchema ? 'Hide' : 'Show'} Schema
                  </Button>
                )}
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
          {!isObjectStore() && showSchema && entityInfo.fields && (
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

      {/* Data Table - Always show this card */}
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle>
                {isObjectStore() ? 'Object Metadata' : 'Data'}
              </CardTitle>
              {queryResult && (
                <CardDescription>
                  Showing {queryResult.returned_count} of{' '}
                  {queryResult.total_count || '?'}{' '}
                  {isObjectStore() ? 'objects' : 'rows'}
                  {appliedFilter !== undefined && ' (filtered)'} • Execution
                  time: {queryResult.execution_time_ms}ms
                </CardDescription>
              )}
            </div>
            {queryLoading && (
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                <span>Loading...</span>
              </div>
            )}
          </div>

          {/* Show error if query failed */}
          {queryError && errorTitle && errorDetail && (
            <Alert variant="destructive" className="mt-4">
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                <div className="space-y-1">
                  <p className="font-semibold">{errorTitle}</p>
                  <p className="text-sm">{errorDetail}</p>
                </div>
              </AlertDescription>
            </Alert>
          )}
          {/* Filter Input - Only show if filtering is supported */}
          {hasFilterSupport && (
            <div className="mt-4 space-y-3">
              {/* Show schema-based filter builder if filter_schema exists */}
              {hasFilterSchema && explorerSupport?.filter_schema ? (
                <DynamicFilterBuilder
                  schema={explorerSupport.filter_schema}
                  formData={filterFormData}
                  onFormDataChange={onFilterFormDataChange}
                  onApplyFilter={onApplyFilter}
                />
              ) : (
                /* Show simple text input for SQL WHERE clause */
                <div className="relative flex-1">
                  <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                  <input
                    type="text"
                    placeholder={
                      hasSqlCapability
                        ? 'Filter data (SQL WHERE clause)...'
                        : 'Filter data (server-side search)...'
                    }
                    value={dataFilterInput}
                    onChange={(e) => onDataFilterInputChange(e.target.value)}
                    onKeyDown={(e) => {
                      // Apply filter on Enter (with or without Ctrl/Cmd)
                      if (e.key === 'Enter') {
                        onApplyFilter()
                      }
                    }}
                    className="w-full pl-10 pr-4 py-2.5 text-sm border rounded-md bg-background focus:outline-none focus:ring-2 focus:ring-ring"
                  />
                </div>
              )}

              {/* Action buttons */}
              <div className="flex gap-2">
                <Button
                  onClick={onApplyFilter}
                  disabled={
                    hasFilterSchema
                      ? Object.keys(filterFormData).length === 0
                      : !dataFilterInput.trim()
                  }
                  size="default"
                  className="px-6"
                >
                  Apply Filter
                </Button>
                {appliedFilter !== undefined && (
                  <Button
                    onClick={onClearFilter}
                    variant="outline"
                    size="default"
                    className="gap-2"
                  >
                    <X className="h-4 w-4" />
                    Clear
                  </Button>
                )}
              </div>
            </div>
          )}
          {/* Show info badge about capabilities */}
          {explorerSupport && (
            <div className="flex gap-2 mt-3">
              {explorerSupport.capabilities.map((capability) => (
                <Badge key={capability} variant="secondary" className="text-xs">
                  {capability.toUpperCase()}
                </Badge>
              ))}
            </div>
          )}
        </CardHeader>
        <CardContent>
          {queryResult && queryResult.rows && queryResult.rows.length > 0 ? (
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
                          <button
                            onClick={() => onSort(field.name)}
                            className="flex items-center gap-2 hover:text-foreground transition-colors group w-full"
                          >
                            <span>{field.name}</span>
                            {dataSortField === field.name ? (
                              dataSortOrder === 'asc' ? (
                                <SortAsc className="h-4 w-4" />
                              ) : (
                                <SortDesc className="h-4 w-4" />
                              )
                            ) : (
                              <ArrowUpDown className="h-4 w-4 opacity-0 group-hover:opacity-50 transition-opacity" />
                            )}
                          </button>
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {queryResult.rows.map((row: any, rowIndex: number) => (
                      <tr
                        key={rowIndex}
                        className="border-b last:border-0 hover:bg-muted/30"
                      >
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
                <div className="text-sm text-muted-foreground flex items-center gap-2">
                  <span>
                    Page {page} • Rows {(page - 1) * pageSize + 1} -{' '}
                    {(page - 1) * pageSize + queryResult.returned_count}
                  </span>
                  {appliedFilter !== undefined && (
                    <Badge variant="secondary" className="text-xs">
                      Filtered
                    </Badge>
                  )}
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
                    disabled={
                      !queryResult || queryResult.returned_count < pageSize
                    }
                    onClick={() => onPageChange(page + 1)}
                  >
                    Next
                  </Button>
                </div>
              </div>
            </>
          ) : (
            <div className="text-center py-8 text-sm text-muted-foreground">
              {appliedFilter !== undefined
                ? 'No results match your filter'
                : 'No data found'}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
