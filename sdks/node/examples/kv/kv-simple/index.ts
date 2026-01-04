/**
 * Simple KV Store Example
 *
 * This example demonstrates basic key-value operations using @temps-sdk/kv:
 * - SET: Store a value
 * - GET: Retrieve a value
 * - DEL: Delete a key
 *
 * Environment variables required (Bun loads .env automatically):
 * - TEMPS_API_URL: Your Temps API URL (e.g., https://app.temps.kfs.es)
 * - TEMPS_TOKEN: Your API key or deployment token
 * - TEMPS_PROJECT_ID: Project ID (required for API keys, optional for deployment tokens)
 */

import { kv, createClient, KVError } from '@temps-sdk/kv';

async function main() {
  console.log('🔑 Temps KV Simple Example\n');

  // Method 1: Using the default kv instance (uses env vars automatically)
  console.log('--- Using default kv instance ---\n');

  const testKey = 'example:greeting';
  const testValue = { message: 'Hello from Temps KV!', timestamp: new Date().toISOString() };

  try {
    // SET - Store a value
    console.log(`Setting key "${testKey}"...`);
    const setResult = await kv.set(testKey, testValue);
    console.log(`✅ SET result: ${setResult}\n`);

    // GET - Retrieve the value
    console.log(`Getting key "${testKey}"...`);
    const getValue = await kv.get<typeof testValue>(testKey);
    console.log(`✅ GET result:`, getValue, '\n');

    // SET with expiration (60 seconds)
    const tempKey = 'example:temp';
    console.log(`Setting key "${tempKey}" with 60s expiration...`);
    await kv.set(tempKey, 'This will expire in 60 seconds', { ex: 60 });
    console.log(`✅ SET with EX result: OK`);

    // Check TTL
    const ttl = await kv.ttl(tempKey);
    console.log(`⏱️  TTL for "${tempKey}": ${ttl} seconds\n`);

    // DEL - Delete the keys
    console.log(`Deleting keys "${testKey}" and "${tempKey}"...`);
    const delCount = await kv.del(testKey, tempKey);
    console.log(`✅ DEL result: ${delCount} key(s) deleted\n`);

    // Verify deletion
    const deletedValue = await kv.get(testKey);
    console.log(`Verifying deletion - GET "${testKey}":`, deletedValue, '(should be null)\n');

  } catch (err) {
    if (err instanceof KVError) {
      console.error(`❌ KV Error: ${err.message}`);
      if (err.title) console.error(`   Title: ${err.title}`);
      if (err.detail) console.error(`   Detail: ${err.detail}`);
      if (err.code) console.error(`   Code: ${err.code}`);
      if (err.status) console.error(`   Status: ${err.status}`);
    } else {
      throw err;
    }
  }

  // Method 2: Using a custom client instance
  console.log('--- Using custom client instance ---\n');

  try {
    // When using API keys, projectId is required
    // When using deployment tokens, projectId is embedded in the token
    const client = createClient({
      apiUrl: process.env.TEMPS_API_URL,
      token: process.env.TEMPS_TOKEN,
      // projectId can be set here or via TEMPS_PROJECT_ID env var
      projectId: process.env.TEMPS_PROJECT_ID ? parseInt(process.env.TEMPS_PROJECT_ID, 10) : undefined,
    });

    const counterKey = 'example:counter';

    // Set initial counter
    await client.set(counterKey, 0);
    console.log(`Set counter to 0`);

    // Increment counter
    const count1 = await client.incr(counterKey);
    console.log(`✅ INCR result: ${count1}`);

    const count2 = await client.incr(counterKey);
    console.log(`✅ INCR result: ${count2}`);

    const count3 = await client.incr(counterKey);
    console.log(`✅ INCR result: ${count3}\n`);

    // List keys matching a pattern
    console.log(`Finding keys matching "example:*"...`);
    const matchingKeys = await client.keys('example:*');
    console.log(`✅ KEYS result:`, matchingKeys, '\n');

    // Cleanup
    if (matchingKeys.length > 0) {
      const cleaned = await client.del(...matchingKeys);
      console.log(`🧹 Cleaned up ${cleaned} key(s)`);
    }

  } catch (err) {
    if (err instanceof KVError) {
      console.error(`❌ KV Error: ${err.message}`);
      if (err.title) console.error(`   Title: ${err.title}`);
      if (err.detail) console.error(`   Detail: ${err.detail}`);
      if (err.code) console.error(`   Code: ${err.code}`);
      if (err.status) console.error(`   Status: ${err.status}`);
    } else {
      throw err;
    }
  }

  console.log('\n✨ Example complete!');
}

main().catch(console.error);
