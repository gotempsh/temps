const fs = require('fs');

// Read and parse the OpenAPI spec
const openApiSpec = JSON.parse(fs.readFileSync('./openapi.json', 'utf-8'));

// Read the SDK to get function parameter requirements
const sdkContent = fs.readFileSync('./src/client/sdk.gen.ts', 'utf-8');

// Map to store tag -> operations mapping
const tagOperations = {};

// Parse operations from OpenAPI spec
for (const [path, methods] of Object.entries(openApiSpec.paths)) {
  for (const [method, operation] of Object.entries(methods)) {
    if (operation.operationId && operation.tags) {
      const operationId = operation.operationId;
      // Convert snake_case to camelCase for function name
      const functionName = operationId.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());

      for (const tag of operation.tags) {
        if (!tagOperations[tag]) {
          tagOperations[tag] = [];
        }

        // Check if function requires parameters
        const funcRegex = new RegExp(`export const ${functionName} = .+?\\(options(\\?)?:`);
        const match = sdkContent.match(funcRegex);
        const isOptional = match && match[1] === '?';

        tagOperations[tag].push({
          name: functionName,
          isOptional
        });
      }
    }
  }
}

// Sort tags and operations
const sortedTags = Object.keys(tagOperations).sort();
sortedTags.forEach(tag => {
  tagOperations[tag].sort((a, b) => a.name.localeCompare(b.name));
});

// Generate namespace classes
const namespaceClasses = sortedTags.map(tag => {
  const className = tag.replace(/[\s-]/g, '');
  const methods = tagOperations[tag].map(op => {
    return `    ${op.name} = (options${op.isOptional ? '?' : ''}: Parameters<typeof sdk.${op.name}>[0]) =>
      sdk.${op.name}({ ...options, client: this.client });`;
  }).join('\n\n');

  return `  class ${className} {
    constructor(private client: Client) {}

${methods}
  }`;
}).join('\n\n');

// Generate property initializations
const propertyInits = sortedTags.map(tag => {
  const className = tag.replace(/[\s-]/g, '');
  const propertyName = tag.toLowerCase()
    .replace(/[\s-]/g, '_')
    .replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
  return `    this.${propertyName} = new ${className}(this.client);`;
}).join('\n');

// Generate property declarations
const propertyDeclarations = sortedTags.map(tag => {
  const className = tag.replace(/[\s-]/g, '');
  const propertyName = tag.toLowerCase()
    .replace(/[\s-]/g, '_')
    .replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
  return `  ${propertyName}: ${className};`;
}).join('\n');

// Generate the complete TypeScript file
const clientClass = `import { createClient, createConfig } from './client/client';
import type { Client } from './client/client';
import * as sdk from './client/sdk.gen';

export * from './client/types.gen';

export interface TempsClientConfig {
  baseUrl: string;
  apiKey?: string;
}

export class TempsClient {
  private client: Client;

  // Namespace properties
${propertyDeclarations}

  constructor(config: TempsClientConfig) {
    const clientConfig = createConfig({
      baseUrl: config.baseUrl,
      headers: config.apiKey ? {
        Authorization: \`Bearer \${config.apiKey}\`
      } : undefined
    });

    this.client = createClient(clientConfig);

    // Initialize namespaces
${propertyInits}
  }

  // Namespace classes
${namespaceClasses}

  // Direct client access for advanced usage
  get rawClient() {
    return this.client;
  }
}

export default TempsClient;`;

fs.writeFileSync('./src/index.ts', clientClass);

// Print summary
console.log(`Generated namespaced client with ${sortedTags.length} namespaces:`);
sortedTags.forEach(tag => {
  const propertyName = tag.toLowerCase()
    .replace(/[\s-]/g, '_')
    .replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
  console.log(`  client.${propertyName} - ${tagOperations[tag].length} methods`);
});
