# KV Simple Example

A simple example demonstrating basic key-value operations using `@temps-sdk/kv`.

## Features Demonstrated

- **SET**: Store values (strings, objects, numbers)
- **GET**: Retrieve values with type safety
- **DEL**: Delete one or more keys
- **INCR**: Increment numeric values
- **TTL**: Check time-to-live on keys
- **KEYS**: Find keys matching a pattern
- **Expiration**: Set keys with automatic expiration

## Setup

1. Install dependencies:

```bash
bun install
```

2. Configure environment variables:

```bash
cp .env.example .env
```

3. Edit `.env` with your Temps credentials:

```env
TEMPS_API_URL=https://app.temps.kfs.es
TEMPS_TOKEN=your-api-key-here
# Required when using API keys, optional for deployment tokens
TEMPS_PROJECT_ID=1
```

## Run

```bash
bun run index.ts
# or
bun start
```

## Expected Output

```
🔑 Temps KV Simple Example

--- Using default kv instance ---

Setting key "example:greeting"...
✅ SET result: OK

Getting key "example:greeting"...
✅ GET result: { message: 'Hello from Temps KV!', timestamp: '2024-01-01T00:00:00.000Z' }

Setting key "example:temp" with 60s expiration...
✅ SET with EX result: OK
⏱️  TTL for "example:temp": 60 seconds

Deleting keys "example:greeting" and "example:temp"...
✅ DEL result: 2 key(s) deleted

Verifying deletion - GET "example:greeting": null (should be null)

--- Using custom client instance ---

Set counter to 0
✅ INCR result: 1
✅ INCR result: 2
✅ INCR result: 3

Finding keys matching "example:*"...
✅ KEYS result: [ 'example:counter' ]

🧹 Cleaned up 1 key(s)

✨ Example complete!
```

## Usage Patterns

### Using the default `kv` instance

```typescript
import { kv } from '@temps-sdk/kv';

// Uses TEMPS_API_URL and TEMPS_TOKEN from environment
await kv.set('mykey', 'myvalue');
const value = await kv.get('mykey');
await kv.del('mykey');
```

### Using a custom client

```typescript
import { createClient } from '@temps-sdk/kv';

const client = createClient({
  apiUrl: 'https://app.temps.kfs.es',
  token: 'your-api-key',
  // Required when using API keys, optional for deployment tokens
  projectId: 1,
});

await client.set('mykey', 'myvalue');
```

### Setting expiration

```typescript
// Expire in 60 seconds
await kv.set('temp', 'value', { ex: 60 });

// Expire in 5000 milliseconds
await kv.set('temp', 'value', { px: 5000 });
```

### Conditional set

```typescript
// Only set if key doesn't exist (NX)
await kv.set('key', 'value', { nx: true });

// Only set if key exists (XX)
await kv.set('key', 'newvalue', { xx: true });
```
