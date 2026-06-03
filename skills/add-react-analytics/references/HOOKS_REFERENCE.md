# Hooks API Reference

Complete reference for all `@temps-sdk/react-analytics` hooks, verified against the package's `.d.ts`. Every hook except `useAnalytics` reads from the `TempsAnalyticsProvider` context, so the component must be rendered inside the provider and carry `'use client'`.

## useTrackEvent

```ts
function useTrackEvent(): (eventName: string, data?: Record<string, JsonValue>) => Promise<void>;
```

Returns a function. Call it to send a custom event.

```tsx
'use client';
import { useTrackEvent } from '@temps-sdk/react-analytics';

function ProductPage() {
  const trackEvent = useTrackEvent();
  return (
    <button onClick={() => trackEvent('add_to_cart', { product_id: 'prod_123', price: 29.99, currency: 'USD' })}>
      Add to Cart
    </button>
  );
}
```

## useTempsAnalytics — the context accessor

```ts
function useTempsAnalytics(): {
  trackEvent: (eventName: string, data?: Record<string, JsonValue>) => Promise<void>;
  identify: (userId: string, traits?: Record<string, JsonValue>) => Promise<void> | void;
  trackPageview: () => void;
  enabled: boolean;
};
```

This is the hook to read analytics context from anywhere in the tree.

```tsx
'use client';
import { useTempsAnalytics } from '@temps-sdk/react-analytics';

function Checkout() {
  const { trackEvent, enabled } = useTempsAnalytics();
  // enabled is false on localhost (ignoreLocalhost) or when disabled
  return <button onClick={() => trackEvent('checkout_started')}>Checkout</button>;
}
```

> ⚠️ `identify` is currently a **no-op placeholder** in the SDK. Attach user attributes as `event_data` on events until the identity endpoint ships.
> ⚠️ There is no `reset()` or `getVisitorId()` on this context.

## useAnalytics — standalone generic helper (NOT the accessor)

```ts
function useAnalytics(options: { client: AnalyticsClient; defaultContext?: Record<string, unknown> }): {
  track: (eventName: string, payload?: Record<string, unknown>) => void | Promise<void>;
  identify: (userId: string, traits?: Record<string, unknown>) => void | Promise<void>;
};
```

This is a generic wrapper around an arbitrary `AnalyticsClient` you supply. It is **not** wired to the Temps provider and **requires a `client` argument** (calling `useAnalytics()` with no args throws). For app analytics use `useTempsAnalytics()` instead. Only reach for `useAnalytics` if you are adapting a custom client.

## useTrackPageview

```ts
function useTrackPageview(): () => void;
```

Returns a function for manual pageview tracking (useful with custom routers).

```tsx
'use client';
import { useTrackPageview } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';
import { usePathname } from 'next/navigation';

function PageViewTracker() {
  const pathname = usePathname();
  const trackPageview = useTrackPageview();
  useEffect(() => { trackPageview(); }, [pathname, trackPageview]);
  return null;
}
```

## usePageLeave

```ts
function usePageLeave(options?: {
  eventName?: string;                       // default "page_leave"
  eventData?: Record<string, JsonValue>;
  enabled?: boolean;                        // default true
}): { triggerPageLeave: () => Promise<void> | undefined };
```

Tracks when the user leaves the page (uses `sendBeacon`). Returns `{ triggerPageLeave }` to fire it manually.

```tsx
'use client';
import { usePageLeave } from '@temps-sdk/react-analytics';

function ArticlePage({ articleId }: { articleId: string }) {
  usePageLeave({ eventName: 'article_leave', eventData: { article_id: articleId } });
  return <article>…</article>;
}
```

> ⚠️ It does **not** take a callback argument (`usePageLeave(data => ...)` is wrong). Configure via the options object; use the returned `triggerPageLeave` to fire manually.

## useEngagementTracking

```ts
function useEngagementTracking(options?: {
  heartbeatInterval?: number;     // default 30000
  inactivityTimeout?: number;     // default 30000
  engagementThreshold?: number;   // default 10000
  enabled?: boolean;              // default true
  onEngagementUpdate?: (data: EngagementData) => void;
  onPageLeave?: (data: EngagementData) => void;
}): { engagementData: EngagementData; isTracking: boolean };

interface EngagementData {
  engagement_time_seconds: number;
  total_time_seconds: number;
  heartbeat_count: number;
  is_engaged: boolean;
  is_visible: boolean;
  time_since_last_activity: number;
}
```

```tsx
'use client';
import { useEngagementTracking } from '@temps-sdk/react-analytics';

function VideoPage() {
  const { engagementData, isTracking } = useEngagementTracking({
    heartbeatInterval: 15000,
    onEngagementUpdate: (d) => console.log('engaged', d.engagement_time_seconds),
  });
  return <video src="/video.mp4" controls />;
}
```

> Note: callback fields are `engagement_time_seconds` / `total_time_seconds` (snake_case), not `engagementTime` / `totalEngagementTime`.

## useSpeedAnalytics

```ts
function useSpeedAnalytics(options?: { basePath?: string; disabled?: boolean }): void;
```

Captures Core Web Vitals (TTFB, LCP, FID, FCP, CLS, INP) and sends them to `${basePath}/speed`. Returns `void` — there is **no** `onMetric` callback. The provider already calls this when `autoTrackSpeedAnalytics` is true; use the hook directly only if you opted out at the provider level.

```tsx
'use client';
import { useSpeedAnalytics } from '@temps-sdk/react-analytics';

function App({ children }: { children: React.ReactNode }) {
  useSpeedAnalytics({ basePath: '/api/_temps' });
  return <>{children}</>;
}
```

## useScrollVisibility

```ts
function useScrollVisibility(options?: {
  eventName?: string;                     // default "component_visible"
  eventData?: Record<string, JsonValue>;
  threshold?: number;                     // 0.0–1.0, default 0.5
  root?: Element | null;                  // default null (viewport)
  rootMargin?: string;                    // default "0px"
  once?: boolean;                         // default true
  enabled?: boolean;                      // default true
}): (node: HTMLElement | null) => void;   // ref callback
```

Returns a **ref callback** to attach to the element you want to track. It fires the event via Intersection Observer when the element becomes visible.

```tsx
'use client';
import { useScrollVisibility } from '@temps-sdk/react-analytics';

function ProductCard({ product }: { product: { id: string; name: string } }) {
  const ref = useScrollVisibility({
    eventName: 'product_viewed',
    eventData: { product_id: product.id, product_name: product.name },
    threshold: 0.75,
    once: true,
  });
  return <div ref={ref} className="product-card">{product.name}</div>;
}
```

> ⚠️ The signature is a **single options object**, not `useScrollVisibility('name', data, opts)`. `threshold` is a single number (0.0–1.0), not an array.

## Session recording hooks

See [SESSION_RECORDING.md](SESSION_RECORDING.md) for `SessionRecordingProvider`, `useSessionRecording`, and `useSessionRecordingControl`.
