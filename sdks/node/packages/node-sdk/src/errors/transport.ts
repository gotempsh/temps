import type { Event, Transport } from './types.js';

export interface TransportOptions {
  url: string;
  headers?: Record<string, string>;
  timeout?: number;
}

export class HttpTransport implements Transport {
  private url: string;
  private headers: Record<string, string>;
  private timeout: number;

  constructor(options: TransportOptions) {
    this.url = options.url;
    this.headers = {
      'Content-Type': 'application/json',
      ...options.headers,
    };
    this.timeout = options.timeout || 30000;
  }

  async sendEvent(event: Event): Promise<void> {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), this.timeout);

    try {
      const response = await fetch(this.url, {
        method: 'POST',
        headers: this.headers,
        body: JSON.stringify(event),
        signal: controller.signal,
      });

      if (!response.ok) {
        throw new Error(`Failed to send event: ${response.status} ${response.statusText}`);
      }
    } catch (error) {
      if (error instanceof Error) {
        if (error.name === 'AbortError') {
          throw new Error(`Request timeout after ${this.timeout}ms`);
        }
        throw error;
      }
      throw new Error('Unknown error occurred while sending event');
    } finally {
      clearTimeout(timeoutId);
    }
  }
}

export class ConsoleTransport implements Transport {
  async sendEvent(event: Event): Promise<void> {
    console.log('[ErrorTracking]', JSON.stringify(event, null, 2));
  }
}

export function parseDsn(dsn: string): { protocol: string; host: string; projectId: string; publicKey: string } {
  const regex = /^(https?):\/\/([^@]+)@([^/]+)\/(\d+)$/;
  const match = dsn.match(regex);

  if (!match) {
    throw new Error('Invalid DSN format');
  }

  const [, protocol, publicKey, host, projectId] = match;

  return {
    protocol,
    publicKey,
    host,
    projectId,
  };
}

export function createTransportFromDsn(dsn: string, debug: boolean = false): Transport {
  if (debug) {
    return new ConsoleTransport();
  }

  const { protocol, host, projectId, publicKey } = parseDsn(dsn);
  const url = `${protocol}://${host}/api/${projectId}/store/`;

  return new HttpTransport({
    url,
    headers: {
      'X-Sentry-Auth': `Sentry sentry_key=${publicKey}, sentry_version=7`,
    },
  });
}
