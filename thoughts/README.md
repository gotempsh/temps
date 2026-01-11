# Service & Container API Documentation

This directory contains comprehensive documentation for service and container management APIs in the Temps web application.

## Documents

### 1. SERVICE_API_ENDPOINTS.md
**Complete API Reference**

Covers all REST endpoints, data types, and React Query hooks for:
- Service management (create, import, get parameters)
- Container management (list, get details, actions)
- Type definitions and data structures
- Workflow examples

**Best for:**
- Understanding API structure
- Reference for endpoint specifications
- Type definitions and response formats
- High-level workflow explanations

### 2. SERVICE_API_CODE_EXAMPLES.md
**Real-World Code Implementation**

Shows actual implementations from the codebase:
- Complete component code (CreateServiceDialog, CreateServiceForm)
- Container action handling (ContainerActionDialog)
- Integration patterns with parent components
- Direct API usage without components
- Error handling patterns
- Advanced custom hooks

**Best for:**
- Copy-paste ready code examples
- Understanding how to integrate with existing code
- Best practices and patterns
- Error handling strategies
- Custom hook creation

## Quick Navigation

### Finding Service Creation Code
```
SERVICE_API_CODE_EXAMPLES.md
├── Section 1: Creating a Service (Full Workflow)
├── Section 3: Integration with Parent Components
└── Section 6: Advanced Custom Hooks
```

### Finding Container Management Code
```
SERVICE_API_CODE_EXAMPLES.md
├── Section 2: Container Management
├── Section 3: Using ContainerActionDialog
└── Section 6: Custom Hook for Container Management
```

### Finding API Details
```
SERVICE_API_ENDPOINTS.md
├── Section 1: API Endpoints
├── Section 3: React Query Hooks
├── Section 4: Type Definitions
└── Section 5: Workflow Examples
```

## Key Components Summary

| Component | Location | Purpose |
|-----------|----------|---------|
| CreateServiceDialog | src/components/storage/CreateServiceDialog.tsx | Service creation wrapper |
| CreateServiceForm | src/components/storage/CreateServiceForm.tsx | Dynamic service form |
| ContainerActionDialog | src/components/containers/ContainerActionDialog.tsx | Container lifecycle actions |

## Key API Functions

| Function | Endpoint | Method |
|----------|----------|--------|
| createService | POST /external-services | Create new service |
| getServiceTypeParameters | GET /external-services/types/{type}/parameters | Fetch form parameters |
| importExternalService | POST /external-services/import | Import running container |
| listContainers | GET /projects/{id}/environments/{id}/containers | List project containers |
| startContainer | POST /projects/{id}/environments/{id}/containers/{id}/start | Start container |
| stopContainer | POST /projects/{id}/environments/{id}/containers/{id}/stop | Stop container |
| restartContainer | POST /projects/{id}/environments/{id}/containers/{id}/restart | Restart container |

## Service Types

Supported service types throughout the API:
- **mongodb** - MongoDB database
- **postgres** - PostgreSQL database
- **redis** - Redis cache
- **s3** - S3 storage

## Quick Start

### Creating a Service
```typescript
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'

<CreateServiceDialog
  open={isOpen}
  onOpenChange={setIsOpen}
  serviceType="postgres"
  onSuccess={(data) => {
    // Handle successful creation
  }}
/>
```

### Managing Containers
```typescript
import { ContainerActionDialog } from '@/components/containers/ContainerActionDialog'

<ContainerActionDialog
  projectId="123"
  environmentId="456"
  action={action}
  containerId={containerId}
  onClose={() => setAction(null)}
/>
```

## Architecture Patterns

### Form Generation
Services use dynamic form generation:
1. Fetch parameters from `getServiceTypeParameters()`
2. Parameters returned as JSON Schema
3. Form schema automatically built with Zod
4. Fields auto-detect as password/number/text
5. Submit via `createService()` mutation

### Container Lifecycle
Container management follows React Query patterns:
1. Load containers with `listContainers()` query
2. Execute action with appropriate mutation
3. Query invalidation refreshes data
4. Toast notification provides feedback

### Error Handling
All mutations include error handling:
- Network errors detected
- Status code-based error messages
- Toast notifications for user feedback
- Query invalidation on success

## File Organization

```
/api/client/
├── sdk.gen.ts                    # API function implementations
├── types.gen.ts                  # TypeScript type definitions
└── @tanstack/react-query.gen.ts # React Query hooks

/components/
├── storage/
│   ├── CreateServiceDialog.tsx
│   └── CreateServiceForm.tsx
└── containers/
    └── ContainerActionDialog.tsx
```

## Technology Stack

- **React Query** - Server state management and data fetching
- **React Hook Form** - Form state and validation
- **Zod** - Schema validation
- **shadcn/ui** - UI components
- **Sonner** - Toast notifications
- **TypeScript** - Type safety

## Notes

- All API functions are auto-generated from OpenAPI spec
- Type safety is enforced throughout with TypeScript
- React Query provides automatic caching and invalidation
- Forms support dynamic field generation
- All operations include proper error handling
- Components are fully reusable and composable

## Related Documentation

- CLAUDE.md - Project development guidelines
- API type definitions: src/api/client/types.gen.ts
- React Query functions: src/api/client/@tanstack/react-query.gen.ts
