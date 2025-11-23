# Service & Container API Search Results

## Summary

Comprehensive search results for service-related API endpoints and container management components in the Temps web application. All findings are auto-generated from the OpenAPI client.

---

## 1. API ENDPOINTS

### 1.1 Service Management Endpoints

#### CREATE SERVICE
**Endpoint:** `POST /external-services`
**Function:** `createService()`
**Location:** `src/api/client/sdk.gen.ts:1111`
**React Query Hook:** `createServiceMutation()` at `src/api/client/@tanstack/react-query.gen.ts:1497`

**Request Type:**
```typescript
CreateServiceData {
  body: CreateExternalServiceRequest;
  path?: never;
  query?: never;
  url: '/external-services';
}
```

**Request Body Structure:**
```typescript
CreateExternalServiceRequest {
  name: string;                          // Service name
  service_type: ServiceTypeRoute;         // 'mongodb' | 'postgres' | 'redis' | 's3'
  parameters: Record<string, unknown>;    // Service-specific parameters
  version?: string | null;                // Optional version override
}
```

**Response:**
```typescript
CreateServiceResponse = ExternalServiceInfo {
  id: number;
  name: string;
  service_type: ServiceTypeRoute;
  status: string;
  created_at: string;
  updated_at: string;
  connection_info?: string | null;
  version?: string | null;
}
```

**Status Codes:**
- `201` - Service created successfully
- `400` - Invalid request
- `500` - Internal server error

---

#### GET SERVICE TYPE PARAMETERS
**Endpoint:** `GET /external-services/types/{service_type}/parameters`
**Function:** `getServiceTypeParameters()`
**Location:** `src/api/client/sdk.gen.ts:1221`
**React Query Hook:** `getServiceTypeParametersOptions()` at `src/api/client/@tanstack/react-query.gen.ts`

**Request:**
```typescript
GetServiceTypeParametersData {
  path: { service_type: string };  // 'mongodb' | 'postgres' | 'redis' | 's3'
  url: '/external-services/types/{service_type}/parameters';
}
```

**Response:** JSON Schema or parameter array describing required/optional fields for the service type

**Status Codes:**
- `200` - Service type parameter schema
- `404` - Service type not found
- `500` - Internal server error

**Usage Example** (from `CreateServiceForm.tsx`):
```typescript
const { data: parametersResponse, isLoading: isLoadingParameters } = useQuery(
  {
    ...getServiceTypeParametersOptions({
      path: {
        service_type: serviceType,  // e.g., 'postgres'
      },
    }),
  }
)
```

---

#### IMPORT EXTERNAL SERVICE
**Endpoint:** `POST /external-services/import`
**Function:** `importExternalService()`
**Location:** `src/api/client/sdk.gen.ts:1151`
**React Query Hook:** `importExternalServiceMutation()` at `src/api/client/@tanstack/react-query.gen.ts:1554`

**Request Type:**
```typescript
ImportExternalServiceData {
  body: ImportExternalServiceRequest;
  path?: never;
  query?: never;
  url: '/external-services/import';
}
```

**Request Body Structure:**
```typescript
ImportExternalServiceRequest {
  container_id: string;                  // Container ID or name to import
  name: string;                          // Name to register service as
  service_type: ServiceTypeRoute;         // 'mongodb' | 'postgres' | 'redis' | 's3'
  parameters: Record<string, unknown>;    // Service configuration parameters
  version?: string | null;                // Optional version override
}
```

**Response:**
```typescript
ImportExternalServiceResponse = ExternalServiceInfo
```

**Status Codes:**
- `200` - Service imported successfully
- `400` - Invalid request
- `401` - Unauthorized
- `500` - Internal server error

---

#### LIST AVAILABLE CONTAINERS
**Endpoint:** `GET /external-services/available-containers`
**Function:** `listAvailableContainers()`
**Location:** `src/api/client/sdk.gen.ts:1125`
**React Query Hook:** `listAvailableContainersOptions()` at `src/api/client/@tanstack/react-query.gen.ts`

**Request:** No parameters required

**Response:**
```typescript
Array<AvailableContainerInfo> {
  container_id: string;        // Container ID or name
  container_name: string;      // Display name
  image: string;               // Docker image (e.g., "postgres:17-alpine")
  service_type: ServiceTypeRoute;  // Service type
  version: string;             // Extracted version from image
  is_running: boolean;         // Whether container is running
}
```

**Status Codes:**
- `200` - List of available containers
- `401` - Unauthorized
- `500` - Internal server error

---

### 1.2 Container Management Endpoints

#### LIST CONTAINERS
**Endpoint:** `GET /projects/{project_id}/environments/{environment_id}/containers`
**Function:** `listContainers()`
**Location:** `src/api/client/sdk.gen.ts:3107`
**React Query Hook:** `listContainersOptions()` at `src/api/client/@tanstack/react-query.gen.ts:4342`

**Request:**
```typescript
ListContainersData {
  path: {
    project_id: number;      // Project ID
    environment_id: number;  // Environment ID
  };
  url: '/projects/{project_id}/environments/{environment_id}/containers';
}
```

**Response:**
```typescript
ContainerListResponse {
  containers: Array<ContainerInfoResponse>;
  total: number;
}

ContainerInfoResponse {
  container_id: string;      // Container ID
  container_name: string;    // Container name
  image_name: string;        // Docker image name
  status: string;            // Container status
  created_at: string;        // Creation timestamp
}
```

**Status Codes:**
- `200` - List of containers
- `400` - Not a server-type project
- `404` - Project or environment not found
- `500` - Internal server error

**Component Usage** (from `ContainerActionDialog.tsx`):
```typescript
listContainersOptions({
  path: {
    project_id: parseInt(projectId),
    environment_id: parseInt(environmentId),
  },
})
```

---

#### GET CONTAINER DETAIL
**Endpoint:** `GET /projects/{project_id}/environments/{environment_id}/containers/{container_id}`
**Function:** `getContainerDetail()`
**Location:** `src/api/client/sdk.gen.ts:3123`

**Request:**
```typescript
GetContainerDetailData {
  path: {
    project_id: number;
    environment_id: number;
    container_id: string;
  };
  url: '/projects/{project_id}/environments/{environment_id}/containers/{container_id}';
}
```

**Response:**
```typescript
ContainerDetailResponse
```

**Status Codes:**
- `200` - Container details
- `404` - Container not found
- `500` - Internal server error

---

#### START CONTAINER
**Endpoint:** `POST /projects/{project_id}/environments/{environment_id}/containers/{container_id}/start`
**Function:** `startContainer()`
**React Query Hook:** `startContainerMutation()` at `src/api/client/@tanstack/react-query.gen.ts`

**Status Codes:**
- `200` - Container started successfully
- `404` - Container not found
- `500` - Internal server error

---

#### STOP CONTAINER
**Endpoint:** `POST /projects/{project_id}/environments/{environment_id}/containers/{container_id}/stop`
**Function:** `stopContainer()`
**React Query Hook:** `stopContainerMutation()` at `src/api/client/@tanstack/react-query.gen.ts`

**Status Codes:**
- `200` - Container stopped successfully
- `404` - Container not found
- `500` - Internal server error

---

#### RESTART CONTAINER
**Endpoint:** `POST /projects/{project_id}/environments/{environment_id}/containers/{container_id}/restart`
**Function:** `restartContainer()`
**React Query Hook:** `restartContainerMutation()` at `src/api/client/@tanstack/react-query.gen.ts`

**Status Codes:**
- `200` - Container restarted successfully
- `404` - Container not found
- `500` - Internal server error

---

## 2. REACT COMPONENTS

### 2.1 Service Creation

**Component:** `CreateServiceDialog`
**Location:** `src/components/storage/CreateServiceDialog.tsx`

**Props:**
```typescript
interface CreateServiceDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  serviceType: ServiceTypeRoute;  // 'mongodb' | 'postgres' | 'redis' | 's3'
  onSuccess: (data: CreateServiceResponse) => void;
}
```

**Usage Pattern:**
```tsx
<CreateServiceDialog
  open={isDialogOpen}
  onOpenChange={setIsDialogOpen}
  serviceType={selectedType}
  onSuccess={(data) => {
    // Handle successful service creation
  }}
/>
```

---

**Component:** `CreateServiceForm`
**Location:** `src/components/storage/CreateServiceForm.tsx`

**Props:**
```typescript
interface CreateServiceFormProps {
  serviceType: ServiceTypeRoute;
  onCancel: () => void;
  onSuccess: (data: CreateServiceResponse) => void;
}
```

**Key Features:**
- Dynamic form generation based on service type parameters
- Automatic parameter loading via `getServiceTypeParametersOptions()`
- Zod schema validation
- Automatic field type detection (password, number, text)
- Service name auto-generation with nanoid (`{serviceType}-{random}`)
- Form state management with React Hook Form

**Form Schema Generation:**
```typescript
// Parameters with encrypted fields are detected automatically
const isEncrypted =
  key.toLowerCase().includes('password') ||
  key.toLowerCase().includes('secret')

// Type detection:
const fieldType =
  prop.type === 'integer' ? 'number' : 'string'
```

---

### 2.2 Container Management

**Component:** `ContainerActionDialog`
**Location:** `src/components/containers/ContainerActionDialog.tsx`

**Props:**
```typescript
interface ContainerActionDialogProps {
  projectId: string;
  environmentId: string;
  action: 'start' | 'stop' | 'restart' | null;
  containerId: string | null;
  onClose: () => void;
  onSuccess?: () => void;
}
```

**Supported Actions:**
- `start` - Start container
- `stop` - Stop container
- `restart` - Restart container

**Query Invalidation:**
Automatically invalidates these queries after action:
- `listContainersOptions()` - Refreshes container list
- `getContainerDetailOptions()` - Refreshes container details

---

## 3. REACT QUERY HOOKS

### Service Mutations
```typescript
// Create service
const createServiceMutation = () => UseMutationOptions<
  CreateServiceResponse,
  DefaultError,
  Options<CreateServiceData>
>

// Import service
const importExternalServiceMutation = () => UseMutationOptions<
  ImportExternalServiceResponse,
  DefaultError,
  Options<ImportExternalServiceData>
>
```

### Container Mutations
```typescript
// Container actions
startContainerMutation()
stopContainerMutation()
restartContainerMutation()
```

### Container Queries
```typescript
// List containers for project
listContainersOptions(options: Options<ListContainersData>)

// Get container details
getContainerDetailOptions(options: Options<GetContainerDetailData>)

// List available containers for import
listAvailableContainersOptions()
```

---

## 4. TYPE DEFINITIONS

### Service Types
```typescript
// Route type for service selection
type ServiceTypeRoute = 'mongodb' | 'postgres' | 'redis' | 's3'

// Response when service created/imported
interface ExternalServiceInfo {
  id: number;
  name: string;
  service_type: ServiceTypeRoute;
  status: string;
  created_at: string;
  updated_at: string;
  connection_info?: string | null;
  version?: string | null;
}
```

### Container Types
```typescript
// Container info in list
interface ContainerInfoResponse {
  container_id: string;
  container_name: string;
  image_name: string;
  status: string;
  created_at: string;
}

// Container list response
interface ContainerListResponse {
  containers: Array<ContainerInfoResponse>;
  total: number;
}

// Available container for import
interface AvailableContainerInfo {
  container_id: string;
  container_name: string;
  image: string;
  service_type: ServiceTypeRoute;
  version: string;
  is_running: boolean;
}
```

---

## 5. WORKFLOW EXAMPLES

### Creating a Service

**Step 1:** Fetch service type parameters
```typescript
const { data: parameters } = useQuery(
  getServiceTypeParametersOptions({
    path: { service_type: 'postgres' }
  })
)
```

**Step 2:** Generate form schema dynamically
```typescript
// Form auto-detects:
// - Required fields from parameter.required
// - Encrypted fields (password/secret in name)
// - Field types (integer → number, etc.)
// - Validation patterns
```

**Step 3:** Submit to create service
```typescript
const result = await createServiceMutation({
  body: {
    name: 'my-postgres',
    service_type: 'postgres',
    parameters: {
      username: 'admin',
      password: '***',
      // ... other parameters
    }
  }
})
```

---

### Importing a Container as Service

**Step 1:** List available containers
```typescript
const { data: containers } = useQuery(
  listAvailableContainersOptions()
)
```

**Step 2:** User selects container to import

**Step 3:** Import with parameters
```typescript
const result = await importExternalServiceMutation({
  body: {
    container_id: 'postgres-1',
    name: 'my-imported-postgres',
    service_type: 'postgres',
    parameters: { /* extracted from running container */ }
  }
})
```

---

### Container Lifecycle Management

**List Containers:**
```typescript
const { data: containerList } = useQuery(
  listContainersOptions({
    path: {
      project_id: 123,
      environment_id: 456
    }
  })
)
```

**Perform Action (e.g., Start):**
```typescript
const mutation = useMutation({
  mutationFn: async () => {
    const options = startContainerMutation()
    return await options.mutationFn({
      path: {
        project_id: 123,
        environment_id: 456,
        container_id: 'container-1'
      }
    })
  },
  onSuccess: () => {
    // Invalidate queries to refresh container list
    queryClient.invalidateQueries({
      queryKey: listContainersOptions({
        path: { project_id: 123, environment_id: 456 }
      }).queryKey
    })
  }
})
```

---

## 6. KEY INTEGRATION POINTS

### 6.1 Form Generation (`CreateServiceForm`)
The form component:
1. Loads parameters using `getServiceTypeParametersOptions()`
2. Handles JSON Schema format from backend
3. Auto-detects encrypted fields
4. Auto-detects numeric fields
5. Applies Zod validation
6. Manages form state with React Hook Form
7. Submits via `createServiceMutation()`

### 6.2 Container Actions (`ContainerActionDialog`)
The dialog component:
1. Displays confirmation before action
2. Uses appropriate mutation (start/stop/restart)
3. Shows loading state with `isPending`
4. Invalidates both list and detail queries
5. Provides user feedback via toast

### 6.3 Service Types
Supported service types throughout:
- **mongodb** - MongoDB database
- **postgres** - PostgreSQL database
- **redis** - Redis cache
- **s3** - S3 storage

---

## 7. FILE LOCATIONS SUMMARY

| Component/Type | Location |
|---|---|
| API SDK Functions | `src/api/client/sdk.gen.ts` |
| React Query Hooks | `src/api/client/@tanstack/react-query.gen.ts` |
| Type Definitions | `src/api/client/types.gen.ts` |
| CreateServiceDialog | `src/components/storage/CreateServiceDialog.tsx` |
| CreateServiceForm | `src/components/storage/CreateServiceForm.tsx` |
| ContainerActionDialog | `src/components/containers/ContainerActionDialog.tsx` |
| Container Management | `src/components/containers/ContainerManagement.tsx` |

---

## NOTES

- All APIs require Bearer token authentication (except `createService`)
- Container actions (start/stop/restart) require project_id, environment_id, and container_id
- Service parameters vary by type and are fetched dynamically
- Form generation is fully automatic based on parameter schema
- All date fields are ISO 8601 formatted
- Component uses shadcn/ui for dialogs and forms
- React Query is used for all data fetching and mutations
