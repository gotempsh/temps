<p align="center">
  <img src="https://raw.githubusercontent.com/gotempsh/temps/refs/heads/main/web/public/logo/temps-logo-light.png" alt="Temps Platform" width="700" />
</p>

<h1 align="center">@temps-sdk/react-analytics</h1>

<p align="center">
  <a href="https://www.npmjs.com/package/@temps-sdk/react-analytics"><img src="https://img.shields.io/npm/v/@temps-sdk/react-analytics.svg" alt="npm version" /></a>
  <a href="https://www.npmjs.com/package/@temps-sdk/react-analytics"><img src="https://img.shields.io/npm/dm/@temps-sdk/react-analytics.svg" alt="npm downloads" /></a>
  <a href="https://github.com/AnomalyCo/temps/blob/main/LICENSE"><img src="https://img.shields.io/npm/l/@temps-sdk/react-analytics.svg" alt="license" /></a>
</p>

<p align="center">
  Privacy-friendly analytics, Web Vitals, session replay, and engagement tracking for React. One provider, zero config, full insight into how users experience your product.
</p>

---

```bash
# npm
npm install @temps-sdk/react-analytics

# bun
bun add @temps-sdk/react-analytics

# pnpm
pnpm add @temps-sdk/react-analytics

# yarn
yarn add @temps-sdk/react-analytics
```

**Peer dependencies:** `react >= 18`

## Quick Start

Wrap your app with the provider -- analytics start working immediately:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return (
    <TempsAnalyticsProvider>
      {children}
    </TempsAnalyticsProvider>
  );
}
```

**What you get out of the box:**

| Feature | Description |
|---------|-------------|
| Pageview tracking | Automatic on `pushState` / `popstate` route changes |
| Page leave events | Time-on-page captured via `pagehide` + `beforeunload` |
| Web Vitals | LCP, FID, CLS, TTFB, FCP, INP -- sent automatically |
| Engagement tracking | Heartbeat-based active time, visibility, and inactivity detection |

## Provider Configuration

```tsx
<TempsAnalyticsProvider
  // Core
  basePath="/api/_temps"            // API endpoint (default). Can be an absolute
                                     // URL on another origin -- see "Externally
                                     // Hosted Apps" below
  ingestKey={undefined}             // Keyed-ingest credential, only needed when
                                     // Temps didn't deploy this app
  domain="example.com"              // Override detected hostname
  disabled={false}                  // Kill switch for analytics

  // Environment
  ignoreLocalhost={true}            // Skip localhost and test environments (default)

  // Pageviews
  autoTrackPageviews={true}         // Track on pushState/popstate (default)
  autoTrackPageLeave={true}         // Track page_leave events (default)
  pageLeaveEventName="page_leave"   // Custom event name for page leave

  // Performance
  autoTrackSpeedAnalytics={true}    // Web Vitals: LCP, FID, CLS, TTFB, FCP, INP (default)

  // Engagement
  autoTrackEngagement={true}        // Heartbeat-based engagement metrics (default)
  heartbeatInterval={30000}         // Heartbeat every 30s (default)
  inactivityTimeout={30000}         // Mark inactive after 30s of no interaction (default)
  engagementThreshold={10000}       // Consider engaged after 10s (default)

  // Session Recording
  enableSessionRecording={false}    // Off by default
  sessionRecordingConfig={{
    excludedPaths: ['/settings/*', '/admin/*'],
    sessionSampleRate: 1.0,         // Record 100% of sessions (default)
    maskAllInputs: true,            // Mask all input fields (default)
    maskTextSelector: '[data-mask]', // CSS selector for text masking
    blockClass: 'rr-block',         // CSS class to block from recording
    ignoreClass: 'rr-ignore',       // CSS class to ignore
    maskTextClass: 'rr-mask',       // CSS class to mask text
    recordCanvas: false,            // Record canvas elements (default: false)
    collectFonts: false,            // Collect font data (default: false)
    batchSize: 100,                 // Events per batch (default)
    flushInterval: 5000,            // Flush interval in ms (default)
  }}
>
  {children}
</TempsAnalyticsProvider>
```

## Custom Events

### Using the Hook

```tsx
import { useTempsAnalytics } from '@temps-sdk/react-analytics';

function PricingPage() {
  const { trackEvent } = useTempsAnalytics();

  return (
    <button onClick={() => trackEvent('plan_selected', { plan: 'pro', billing: 'annual' })}>
      Choose Pro
    </button>
  );
}
```

### Using the Shorthand Hook

```tsx
import { useTrackEvent } from '@temps-sdk/react-analytics';

function DownloadButton() {
  const track = useTrackEvent();

  return (
    <button onClick={() => track('file_downloaded', { format: 'pdf' })}>
      Download PDF
    </button>
  );
}
```

### Using HTML Attributes (Zero JS)

Track events declaratively without writing any handler code:

```html
<button temps-event-name="cta_click" temps-data-section="hero" temps-data-variant="blue">
  Get Started
</button>
```

Any element with `temps-event-name` fires an event on click. All `temps-data-*` attributes are sent as event properties.

## Hooks Reference

### `useTempsAnalytics()`

The core hook. Returns the full analytics context.

```typescript
const { trackEvent, trackPageview, identify, enabled } = useTempsAnalytics();
```

| Property | Type | Description |
|----------|------|-------------|
| `trackEvent` | `(name: string, data?: Record<string, JsonValue>) => Promise<void>` | Send a custom event |
| `trackPageview` | `() => void` | Manually fire a pageview |
| `identify` | `(userId: string, traits?) => void` | Identify a user (placeholder for future use) |
| `enabled` | `boolean` | Whether analytics are currently active |

### `useTrackEvent()`

Convenience hook that returns just the `trackEvent` function.

```typescript
const track = useTrackEvent();
track('button_clicked', { id: 'cta-hero' });
```

### `useTrackPageview()`

Returns a function to manually trigger a pageview.

```typescript
const trackPageview = useTrackPageview();

useEffect(() => {
  trackPageview(); // Fire on mount
}, []);
```

### `usePageLeave(options?)`

Track when users leave a page, including time spent.

```tsx
import { usePageLeave } from '@temps-sdk/react-analytics';

function ArticlePage() {
  const { triggerPageLeave } = usePageLeave({
    eventName: 'article_leave',       // Default: 'page_leave'
    eventData: { category: 'blog' },  // Extra data merged into the event
    enabled: true,                     // Default: true
  });

  // Fires automatically on pagehide/beforeunload.
  // Or trigger manually before client-side navigation:
  const handleNavigation = () => {
    triggerPageLeave();
    router.push('/next-page');
  };
}
```

The event payload automatically includes `time_on_page_ms`, `url`, `referrer`, and `timestamp`.

### `useSpeedAnalytics(options?)`

Tracks [Web Vitals](https://web.dev/vitals/) performance metrics. Enabled automatically by the provider, but can be used standalone.

```typescript
import { useSpeedAnalytics } from '@temps-sdk/react-analytics';

useSpeedAnalytics({
  basePath: '/api/_temps',
  disabled: false,
});
```

**Metrics collected:**

| Metric | Full Name | Description |
|--------|-----------|-------------|
| **TTFB** | Time to First Byte | Server responsiveness |
| **FCP** | First Contentful Paint | First pixels on screen |
| **LCP** | Largest Contentful Paint | Main content visible |
| **FID** | First Input Delay | Input responsiveness |
| **CLS** | Cumulative Layout Shift | Visual stability |
| **INP** | Interaction to Next Paint | Overall responsiveness |

Initial metrics (TTFB, FCP, LCP, FID) are batched into a single request. Late metrics (CLS, INP) are sent individually as they stabilize.

### `useEngagementTracking(options?)`

Manual engagement tracking for specific pages or components. Useful when you need per-component engagement data or custom callbacks.

```tsx
import { useEngagementTracking } from '@temps-sdk/react-analytics';

function LongArticle() {
  const { engagementData, isTracking } = useEngagementTracking({
    heartbeatInterval: 15000,    // 15s heartbeats
    engagementThreshold: 5000,   // Engaged after 5s
    onEngagementUpdate: (data) => {
      console.log(`Active for ${data.engagement_time_seconds}s`);
    },
    onPageLeave: (data) => {
      console.log(`Total time: ${data.total_time_seconds}s`);
    },
  });

  return <article>{/* ... */}</article>;
}
```

**Engagement data shape:**

```typescript
interface EngagementData {
  engagement_time_seconds: number;   // Active time on page
  total_time_seconds: number;        // Total time on page
  heartbeat_count: number;           // Number of heartbeats sent
  is_engaged: boolean;               // Currently engaged
  is_visible: boolean;               // Page is visible (not backgrounded)
  time_since_last_activity: number;  // Seconds since last interaction
}
```

### `useScrollVisibility(options?)`

Track when elements scroll into view using Intersection Observer. Returns a ref callback to attach to any element.

```tsx
import { useScrollVisibility } from '@temps-sdk/react-analytics';

function PricingSection() {
  const ref = useScrollVisibility({
    eventName: 'pricing_viewed',
    eventData: { section: 'pricing' },
    threshold: 0.75,    // 75% of element must be visible
    once: true,          // Fire only once (default)
  });

  return <section ref={ref}>Pricing Plans</section>;
}
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `eventName` | `string` | `'component_visible'` | Event name to fire |
| `eventData` | `Record<string, JsonValue>` | - | Extra data |
| `threshold` | `number` | `0.5` | Visibility percentage (0.0 - 1.0) |
| `root` | `Element \| null` | `null` (viewport) | Scroll container |
| `rootMargin` | `string` | `'0px'` | Margin around root |
| `once` | `boolean` | `true` | Track only the first time |
| `enabled` | `boolean` | `true` | Enable/disable tracking |

### `useSessionRecording()`

Control session recording programmatically from within the provider.

```tsx
import { useSessionRecording } from '@temps-sdk/react-analytics';

function RecordingToggle() {
  const {
    isRecordingEnabled,
    enableRecording,
    disableRecording,
    toggleRecording,
    sessionId,
  } = useSessionRecording();

  return (
    <div>
      <p>Recording: {isRecordingEnabled ? 'ON' : 'OFF'}</p>
      {sessionId && <p>Session: {sessionId}</p>}
      <button onClick={toggleRecording}>Toggle</button>
    </div>
  );
}
```

### `useSessionRecordingControl(defaultEnabled?)`

Standalone recording control without needing the provider context. Persists user preference to `localStorage`.

```typescript
const { isEnabled, enable, disable, toggle } = useSessionRecordingControl(false);
```

### `useAnalytics(options)`

Low-level hook for plugging in a custom analytics client. Useful if you want the hook interface without the Temps provider.

```typescript
import { useAnalytics } from '@temps-sdk/react-analytics';

const track = useAnalytics({
  client: {
    track: (event, data) => myAnalytics.send(event, data),
    identify: (userId) => myAnalytics.identify(userId),
  },
  defaultContext: { app: 'my-app' },
});
```

## Session Recording

Session recording captures DOM mutations and user interactions using [rrweb](https://www.rrweb.io/) and streams them to your Temps instance for replay.

### Enable Recording

```tsx
<TempsAnalyticsProvider
  enableSessionRecording={true}
  sessionRecordingConfig={{
    sessionSampleRate: 0.1,  // Record 10% of sessions
  }}
>
  {children}
</TempsAnalyticsProvider>
```

### Privacy Controls

Recording respects user privacy by default:

```tsx
sessionRecordingConfig={{
  maskAllInputs: true,                // All input values are masked
  maskTextSelector: '[data-mask]',    // Mask specific text elements
  blockClass: 'rr-block',            // Block elements entirely
  ignoreClass: 'rr-ignore',          // Ignore elements from recording
}}
```

**In your markup:**

```html
<!-- All inputs are masked by default (passwords, emails, etc.) -->
<input type="password" />

<!-- This element won't appear in recordings at all -->
<div class="rr-block">Sensitive content</div>

<!-- This text will be replaced with asterisks -->
<p data-mask>User's private note</p>

<!-- This element is ignored (not captured) -->
<aside class="rr-ignore">Debug panel</aside>
```

### Path Exclusions

Exclude specific routes from recording:

```tsx
sessionRecordingConfig={{
  excludedPaths: [
    '/settings/*',    // Wildcard: all settings subpages
    '/admin/*',       // Wildcard: entire admin area
    '/checkout',      // Exact match
  ],
}}
```

Recording automatically pauses when navigating to excluded paths and resumes when leaving them.

### How It Works

1. `SessionRecorder` initializes a session via `POST /api/_temps/session-replay/init` with device metadata
2. DOM mutations are captured by rrweb, packed with `@rrweb/packer`, and batched (100 events or 10s)
3. Batches are base64-encoded and sent to `POST /api/_temps/session-replay/events`
4. On page unload, remaining events are flushed via `navigator.sendBeacon` for reliability
5. Failed sends use exponential backoff (up to 5 retries before dropping)
6. Full DOM snapshots are taken every 30s or every 200 events for replay accuracy

## Standalone Engagement Tracker

For non-React environments or custom integrations, use the `EngagementTracker` class directly:

```typescript
import { EngagementTracker } from '@temps-sdk/react-analytics';

const tracker = new EngagementTracker({
  basePath: '/api/_temps',
  domain: 'example.com',
  heartbeatInterval: 30000,
  inactivityTimeout: 30000,
  engagementThreshold: 10000,
});

// Clean up when done
tracker.destroy();
```

## Framework Setup

### Next.js (App Router)

The SDK ships with `"use client"` directives. Create a client-side provider wrapper:

```tsx
// app/providers.tsx
'use client';

import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export function Providers({ children }: { children: React.ReactNode }) {
  return (
    <TempsAnalyticsProvider
      enableSessionRecording={process.env.NODE_ENV === 'production'}
    >
      {children}
    </TempsAnalyticsProvider>
  );
}

// app/layout.tsx
import { Providers } from './providers';

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html>
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
```

### Vite / Create React App

```tsx
// main.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <TempsAnalyticsProvider>
    <App />
  </TempsAnalyticsProvider>
);
```

## Externally Hosted Apps (Cross-Origin Ingest)

If Temps isn't the platform hosting this app -- e.g. a Next.js app on a separate hosting platform talking to a self-hosted Temps backend on another domain -- point the provider at your Temps backend with an ingest key instead of the default same-origin `basePath`:

```tsx
<TempsAnalyticsProvider
  basePath="https://api.your-domain.example.com/api/_temps"
  ingestKey="pa_..."
>
  {children}
</TempsAnalyticsProvider>
```

Mint the key with `bunx @temps-sdk/cli analytics keys create --project <id>` or from the project's Analytics settings in the Console. Session/visitor identity (`localStorage`-backed, generated automatically by `@temps-sdk/analytics-core`) is attached to every event and is what the backend attributes traffic to on this path, since no Temps-issued cookie exists cross-origin.

For the full walkthrough -- routing through a same-origin proxy so the key never ships to the browser, `sendBeacon` header limitations, and Next.js-specific gotchas -- see [Externally Hosted Apps](https://temps.sh/docs/sdks#external-hosting) in the SDK docs.

## Environment Detection

The SDK automatically disables itself in non-production environments when `ignoreLocalhost` is `true` (default):

| Environment | Detection method |
|---|---|
| Localhost | `localhost`, `127.0.0.1`, `::1`, `*.local` |
| Test runners | `jest`, `mocha`, `vitest`, `cypress` globals |
| Manual override | `localStorage.temps_ignore = "true"` |

## TypeScript

All hooks, components, and types are fully typed and exported:

```typescript
import type {
  AnalyticsContextValue,
  TempsAnalyticsProviderProps,
  EngagementData,
  EngagementTrackerOptions,
  WebVitalMetric,
  SpeedMetric,
  JsonValue,
} from '@temps-sdk/react-analytics';
```

## Requirements

- React 18+ or React 19
- A running Temps instance with the analytics proxy configured

## Related

- [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) -- Key-value store
- [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) -- File storage
- [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) -- Full platform API client and server-side error tracking

## License

MIT
