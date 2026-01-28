---
name: add-node-sdk
description: |
  Integrate the Temps Node.js SDK for server-side analytics, KV storage, blob storage, and platform management. Use when the user wants to: (1) Track server-side events, (2) Use Temps KV (key-value) storage, (3) Use Temps Blob storage for files, (4) Access Temps API from Node.js, (5) Server-side analytics integration, (6) Backend event tracking. Triggers: "temps node sdk", "server-side analytics", "temps kv", "temps blob", "backend integration", "node.js temps".
---

# Add Node.js SDK

Integrate Temps platform features in Node.js applications.

## Installation

```bash
# Core SDK
npm install @temps-sdk/node

# Individual packages (optional)
npm install @temps-sdk/kv      # Key-Value storage
npm install @temps-sdk/blob    # Blob/file storage
```

## Configuration

```typescript
import { Temps } from '@temps-sdk/node';

const temps = new Temps({
  apiKey: process.env.TEMPS_API_KEY,
  projectId: process.env.TEMPS_PROJECT_ID,
});
```

## Server-Side Event Tracking

Track events from your backend:

```typescript
import { Temps } from '@temps-sdk/node';

const temps = new Temps({
  apiKey: process.env.TEMPS_API_KEY,
  projectId: process.env.TEMPS_PROJECT_ID,
});

// Track an event
await temps.track('purchase_completed', {
  userId: 'user_123',
  orderId: 'order_456',
  amount: 99.99,
  currency: 'USD',
  items: ['product_1', 'product_2'],
});

// Identify a user
await temps.identify('user_123', {
  email: 'user@example.com',
  name: 'John Doe',
  plan: 'premium',
  createdAt: new Date().toISOString(),
});
```

### Express Middleware

```typescript
import express from 'express';
import { Temps } from '@temps-sdk/node';

const app = express();
const temps = new Temps({ apiKey: process.env.TEMPS_API_KEY });

// Track all API requests
app.use((req, res, next) => {
  const start = Date.now();

  res.on('finish', () => {
    temps.track('api_request', {
      method: req.method,
      path: req.path,
      statusCode: res.statusCode,
      duration: Date.now() - start,
      userId: req.user?.id,
    });
  });

  next();
});
```

## KV Storage

Simple key-value storage with automatic JSON serialization:

```typescript
import { KV } from '@temps-sdk/kv';

const kv = new KV({
  apiKey: process.env.TEMPS_API_KEY,
  namespace: 'my-app', // Optional namespace
});

// Store values
await kv.set('user:123', { name: 'John', email: 'john@example.com' });
await kv.set('session:abc', { userId: '123' }, { ttl: 3600 }); // 1 hour TTL

// Retrieve values
const user = await kv.get('user:123');
// { name: 'John', email: 'john@example.com' }

// Check existence
const exists = await kv.has('user:123');

// Delete
await kv.delete('user:123');

// List keys
const keys = await kv.list({ prefix: 'user:' });
// ['user:123', 'user:456', ...]
```

### KV Options

```typescript
// Set with TTL (seconds)
await kv.set('key', value, { ttl: 3600 });

// Set with metadata
await kv.set('key', value, {
  metadata: { version: 1, updatedBy: 'system' }
});

// Get with metadata
const { value, metadata } = await kv.getWithMetadata('key');

// List with pagination
const result = await kv.list({
  prefix: 'user:',
  limit: 100,
  cursor: 'next-cursor',
});
```

## Blob Storage

Store and retrieve files:

```typescript
import { Blob } from '@temps-sdk/blob';

const blob = new Blob({
  apiKey: process.env.TEMPS_API_KEY,
});

// Upload file from buffer
const file = await blob.put('avatars/user-123.png', imageBuffer, {
  contentType: 'image/png',
  metadata: { userId: '123' },
});

// Upload from stream
import { createReadStream } from 'fs';
await blob.put('backups/data.json', createReadStream('./data.json'), {
  contentType: 'application/json',
});

// Get file
const data = await blob.get('avatars/user-123.png');

// Get as stream (for large files)
const stream = await blob.getStream('backups/data.json');

// Get signed URL (for client-side access)
const url = await blob.getSignedUrl('avatars/user-123.png', {
  expiresIn: 3600, // 1 hour
});

// Delete
await blob.delete('avatars/user-123.png');

// List files
const files = await blob.list({ prefix: 'avatars/' });
```

### Upload from Client

Generate presigned upload URLs:

```typescript
// Server: Generate upload URL
const uploadUrl = await blob.createUploadUrl('uploads/file.pdf', {
  contentType: 'application/pdf',
  maxSize: 10 * 1024 * 1024, // 10MB
  expiresIn: 300, // 5 minutes
});

// Client: Upload directly to storage
await fetch(uploadUrl, {
  method: 'PUT',
  body: file,
  headers: { 'Content-Type': 'application/pdf' },
});
```

## Error Handling

```typescript
import { TempsError, RateLimitError, NotFoundError } from '@temps-sdk/node';

try {
  await temps.track('event', data);
} catch (error) {
  if (error instanceof RateLimitError) {
    // Retry after delay
    await sleep(error.retryAfter * 1000);
    await temps.track('event', data);
  } else if (error instanceof NotFoundError) {
    console.error('Resource not found:', error.message);
  } else if (error instanceof TempsError) {
    console.error('Temps error:', error.message, error.code);
  } else {
    throw error;
  }
}
```

## TypeScript Support

Full TypeScript support with generics:

```typescript
interface User {
  name: string;
  email: string;
  plan: 'free' | 'premium';
}

// Typed KV operations
const user = await kv.get<User>('user:123');
// user is User | null

await kv.set<User>('user:123', {
  name: 'John',
  email: 'john@example.com',
  plan: 'premium',
});
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `TEMPS_API_KEY` | Yes | API key from dashboard |
| `TEMPS_PROJECT_ID` | For tracking | Project ID for analytics |
| `TEMPS_API_URL` | No | Custom API URL (self-hosted) |

## Best Practices

1. **Initialize once**: Create SDK instance at app startup
2. **Use environment variables**: Never hardcode API keys
3. **Handle errors gracefully**: Wrap SDK calls in try/catch
4. **Use namespaces in KV**: Organize keys by feature/domain
5. **Set TTLs appropriately**: Don't store temporary data forever
