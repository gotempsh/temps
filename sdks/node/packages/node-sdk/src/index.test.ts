import { describe, it, expect, beforeEach, vi } from 'vitest';
import { TempsClient } from './index';
import * as clientModule from './client/client';
import * as sdk from './client/sdk.gen';

// Mock the client module
vi.mock('./client/client', () => ({
  createClient: vi.fn(() => ({
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
    patch: vi.fn()
  })),
  createConfig: vi.fn((config) => config)
}));

// Mock the SDK methods
vi.mock('./client/sdk.gen', () => {
  const mockMethod = () => vi.fn().mockResolvedValue({ data: 'mocked' });

  return {
    // Platform methods
    getPlatformInfo: mockMethod(),

    // API Keys methods
    listApiKeys: mockMethod(),
    createApiKey: mockMethod(),
    getApiKeyPermissions: mockMethod(),
    getApiKey: mockMethod(),
    updateApiKey: mockMethod(),
    deleteApiKey: mockMethod(),
    activateApiKey: mockMethod(),
    deactivateApiKey: mockMethod(),

    // Analytics methods
    enrichVisitor: mockMethod(),
    getAnalyticsMetrics: mockMethod(),
    getBrowsers: mockMethod(),
    getEventsCount: mockMethod(),
    getPathVisitors: mockMethod(),
    getReferrers: mockMethod(),
    getSessionDetails: mockMethod(),
    getSessionEvents: mockMethod(),
    getSessionLogs: mockMethod(),
    getSessionMetrics: mockMethod(),
    getStatusCodes: mockMethod(),
    getViewsOverTime: mockMethod(),
    getVisitorDetails: mockMethod(),
    getVisitorLocations: mockMethod(),
    getVisitors: mockMethod(),
    getVisitorSessions: mockMethod(),

    // MCP methods
    listClients: mockMethod(),
    addClient: mockMethod(),
    removeClient: mockMethod(),
    connectClient: mockMethod(),

    // Projects methods
    createProject: mockMethod(),
    getProjects: mockMethod(),
    getProject: mockMethod(),
    updateProject: mockMethod(),
    deleteProject: mockMethod(),

    // Add other methods as they are used in tests
  };
});

describe('TempsClient', () => {
  let client: TempsClient;

  beforeEach(() => {
    vi.clearAllMocks();
    client = new TempsClient({
      baseUrl: 'https://api.test.com',
      apiKey: 'test-api-key'
    });
  });

  describe('Client Initialization', () => {
    it('should create a client with the correct configuration', () => {
      expect(clientModule.createConfig).toHaveBeenCalledWith({
        baseUrl: 'https://api.test.com',
        headers: {
          Authorization: 'Bearer test-api-key'
        }
      });
      expect(clientModule.createClient).toHaveBeenCalled();
    });

    it('should create a client without auth headers when no API key is provided', () => {
      vi.clearAllMocks();
      new TempsClient({
        baseUrl: 'https://api.test.com'
      });

      expect(clientModule.createConfig).toHaveBeenCalledWith({
        baseUrl: 'https://api.test.com',
        headers: undefined
      });
    });

    it('should expose rawClient getter', () => {
      expect(client.rawClient).toBeDefined();
      expect(client.rawClient).toEqual(expect.objectContaining({
        get: expect.any(Function),
        post: expect.any(Function)
      }));
    });
  });

  describe('Namespace Structure', () => {
    it('should have all required namespaces', () => {
      const namespaces = [
        'apiKeys',
        'analytics',
        'auditLogs',
        'authentication',
        'backups',
        'crons',
        'deployments',
        'develop',
        'domains',
        'externalServices',
        'featureFlags',
        'files',
        'funnels',
        'github',
        'loadBalancer',
        'logs',
        'mcp',
        'metrics',
        'notifications',
        'opentelemetry',
        'payments',
        'pipelines',
        'platform',
        'projects',
        'speedInsights',
        'users',
        'websocket'
      ];

      namespaces.forEach(namespace => {
        expect(client).toHaveProperty(namespace);
        expect(client[namespace as keyof TempsClient]).toBeDefined();
      });
    });

    it('should have methods in platform namespace', () => {
      expect(client.platform.getPlatformInfo).toBeDefined();
      expect(typeof client.platform.getPlatformInfo).toBe('function');
    });

    it('should have methods in apiKeys namespace', () => {
      const apiKeysMethods = [
        'listApiKeys',
        'createApiKey',
        'getApiKeyPermissions',
        'getApiKey',
        'updateApiKey',
        'deleteApiKey',
        'activateApiKey',
        'deactivateApiKey'
      ];

      apiKeysMethods.forEach(method => {
        expect(client.apiKeys).toHaveProperty(method);
        expect(typeof client.apiKeys[method as keyof typeof client.apiKeys]).toBe('function');
      });
    });

    it('should have methods in analytics namespace', () => {
      const analyticsMethods = [
        'enrichVisitor',
        'getAnalyticsMetrics',
        'getBrowsers',
        'getEventsCount',
        'getPathVisitors',
        'getReferrers',
        'getSessionDetails',
        'getSessionEvents',
        'getSessionLogs',
        'getSessionMetrics',
        'getStatusCodes',
        'getViewsOverTime',
        'getVisitorDetails',
        'getVisitorLocations',
        'getVisitors',
        'getVisitorSessions'
      ];

      analyticsMethods.forEach(method => {
        expect(client.analytics).toHaveProperty(method);
        expect(typeof client.analytics[method as keyof typeof client.analytics]).toBe('function');
      });
    });

    it('should have methods in mcp namespace', () => {
      const mcpMethods = [
        'listClients',
        'addClient',
        'removeClient',
        'connectClient'
      ];

      mcpMethods.forEach(method => {
        expect(client.mcp).toHaveProperty(method);
        expect(typeof client.mcp[method as keyof typeof client.mcp]).toBe('function');
      });
    });
  });

  describe('Method Invocation', () => {
    it('should call SDK methods with client instance', async () => {
      const mockOptions = { query: { page: 1 } };

      await client.apiKeys.listApiKeys(mockOptions);

      expect(sdk.listApiKeys).toHaveBeenCalledWith({
        ...mockOptions,
        client: client.rawClient
      });
    });

    it('should pass through method parameters correctly', async () => {
      const mockOptions = {
        path: { id: 123 },
        body: { name: 'Test API Key' }
      };

      await client.apiKeys.updateApiKey(mockOptions as any);

      expect(sdk.updateApiKey).toHaveBeenCalledWith({
        ...mockOptions,
        client: client.rawClient
      });
    });

    it('should handle optional parameters', async () => {
      await client.platform.getPlatformInfo();

      expect(sdk.getPlatformInfo).toHaveBeenCalledWith({
        client: client.rawClient
      });
    });

    it('should handle required parameters', async () => {
      const mockOptions = {
        body: { name: 'New Project' }
      };

      await client.projects.createProject(mockOptions as any);

      expect(sdk.createProject).toHaveBeenCalledWith({
        ...mockOptions,
        client: client.rawClient
      });
    });
  });

  describe('Type Safety', () => {
    it('should maintain type safety for method parameters', () => {
      // This test mainly ensures TypeScript compilation works correctly
      // The actual type checking happens at compile time

      // Example: listApiKeys accepts optional parameters
      const validCall1 = () => client.apiKeys.listApiKeys();
      const validCall2 = () => client.apiKeys.listApiKeys({ query: { page: 1, page_size: 20 } } as any);

      expect(validCall1).not.toThrow();
      expect(validCall2).not.toThrow();
    });
  });

  describe('Error Handling', () => {
    it('should propagate errors from SDK methods', async () => {
      const mockError = new Error('API Error');
      vi.mocked(sdk.listApiKeys).mockRejectedValueOnce(mockError);

      await expect(client.apiKeys.listApiKeys()).rejects.toThrow('API Error');
    });
  });
});

describe('TempsClient Integration', () => {
  it('should support method chaining through namespaces', async () => {
    const client = new TempsClient({
      baseUrl: 'https://api.test.com',
      apiKey: 'test-key'
    });

    // Test that we can chain namespace access and method calls
    const platformCall = client.platform.getPlatformInfo();
    const apiKeysCall = client.apiKeys.listApiKeys();
    const analyticsCall = client.analytics.getVisitors({} as any);

    expect(platformCall).toBeDefined();
    expect(apiKeysCall).toBeDefined();
    expect(analyticsCall).toBeDefined();
  });

  it('should handle concurrent requests across different namespaces', async () => {
    const client = new TempsClient({
      baseUrl: 'https://api.test.com',
      apiKey: 'test-key'
    });

    const promises = [
      client.platform.getPlatformInfo(),
      client.apiKeys.listApiKeys(),
      client.analytics.getVisitors({} as any),
      client.mcp.listClients()
    ];

    const results = await Promise.all(promises);

    expect(results).toHaveLength(4);
    results.forEach(result => {
      expect(result).toEqual({ data: 'mocked' });
    });
  });
});
