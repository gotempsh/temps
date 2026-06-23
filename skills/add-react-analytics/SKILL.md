---
name: add-react-analytics
description: |
  Add Temps analytics to React applications with comprehensive tracking capabilities including page views, custom events, scroll tracking, engagement monitoring, session recording, and Web Vitals performance metrics. Use when the user wants to: (1) Add analytics to a React app (Next.js App Router, Next.js Pages Router, Vite, Create React App, or Remix), (2) Track user events or interactions, (3) Monitor scroll depth or element visibility, (4) Add session recording/replay, (5) Track Web Vitals or performance metrics, (6) Measure user engagement or time on page, (7) Set up product analytics or telemetry. Triggers: "add analytics", "track events", "session recording", "web vitals", "user tracking", "temps analytics", "react analytics".
---

# Add React Analytics

Integrate the `@temps-sdk/react-analytics` SDK into a React application.

> **Verified against the real published package.** A prior version of this skill documented props and hooks that do not exist (`autoTrack={{...}}`, `debug`, `useAnalytics()` as the accessor, `reset`, `getVisitorId`) and broke integrations. Before changing any API here, confirm against the package's type definitions:
> ```bash
> npm pack @temps-sdk/react-analytics@latest && tar -xzf temps-sdk-react-analytics-*.tgz \
>   && cat package/dist/index.d.ts package/dist/types.d.ts package/dist/Provider.d.ts
> ```
> Trust the `.d.ts`, not prose.

## Installation

```bash
npm install @temps-sdk/react-analytics
# or: yarn add / pnpm add / bun add
```

Peer deps: React 18 or 19 (`react`, `react-dom`).

## Two things to know before wiring it up

1. **The package already ships `'use client'`** at the top of its build. In the Next.js App Router you import `TempsAnalyticsProvider` **directly into your Server Component `layout.tsx`** — you do **not** need to author your own `'use client'` wrapper component around it.
2. **`ignoreLocalhost` defaults to `true`** → the SDK sends **nothing** while running on localhost. Correct for production, but it means you see no network requests in local dev. Pass `ignoreLocalhost={false}` only when you explicitly want to test locally.

## basePath: what to set

The SDK POSTs to `${basePath}/event`, `${basePath}/speed`, `${basePath}/heartbeat`, and session replay to `${basePath}/session-replay` (via `sendBeacon`, falling back to keepalive `fetch`).

- **App deployed on Temps → no `basePath` is required.** The SDK default is `/api/_temps`, and the Temps proxy treats `/api/_temps/*` as a public ingest path: it bypasses the auth gate from any host and routes to the platform's analytics handlers. **No app-side route handler is needed.**
- **App NOT on Temps** → `${basePath}/...` hits your own origin. You must either run a route that forwards to Temps, or point `basePath` at an absolute Temps ingest URL, and set `domain="<project-domain>"` so events are attributed correctly.

The package's built-in default basePath is `/api/_temps`. Set `basePath` only when the app needs a custom same-origin proxy path.

## Framework Setup

### Next.js App Router (13+)

```tsx
// app/layout.tsx — stays a Server Component; the provider carries its own 'use client'.
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <TempsAnalyticsProvider basePath="/api/_temps">
          {children}
        </TempsAnalyticsProvider>
      </body>
    </html>
  );
}
```

### Next.js Pages Router

```tsx
// pages/_app.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';
import type { AppProps } from 'next/app';

export default function App({ Component, pageProps }: AppProps) {
  return (
    <TempsAnalyticsProvider basePath="/api/_temps">
      <Component {...pageProps} />
    </TempsAnalyticsProvider>
  );
}
```

### Vite / Create React App

```tsx
// src/main.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <TempsAnalyticsProvider basePath="/api/_temps">
    <App />
  </TempsAnalyticsProvider>
);
```

### Remix

```tsx
// app/root.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App() {
  return (
    <html lang="en">
      <body>
        <TempsAnalyticsProvider basePath="/api/_temps">
          <Outlet />
        </TempsAnalyticsProvider>
      </body>
    </html>
  );
}
```

## Provider Configuration — real props (all flat, all optional)

```tsx
<TempsAnalyticsProvider
  basePath="/api/_temps"          // see "basePath" above
  domain={undefined}              // defaults to window.location.hostname
  disabled={false}                // hard off-switch (e.g. for tests)
  ignoreLocalhost={true}          // default true → silent on localhost; set false to test locally
  autoTrackPageviews={true}       // default true
  autoTrackPageLeave={true}       // default true
  pageLeaveEventName="page_leave" // default "page_leave"
  autoTrackSpeedAnalytics={true}  // default true — Web Vitals
  autoTrackEngagement={true}      // default true — heartbeats
  heartbeatInterval={30000}       // default 30000ms
  inactivityTimeout={30000}       // default 30000ms
  engagementThreshold={10000}     // default 10000ms
  enableSessionRecording={false}  // default false — see "Session Recording"
  sessionRecordingConfig={{ /* see below */ }}
>
  {children}
</TempsAnalyticsProvider>
```

> ⚠️ There is **no** nested `autoTrack={{ ... }}` prop and **no** `debug` prop. Old docs that show those are wrong.

## Available Hooks

Full signatures and examples in [HOOKS_REFERENCE.md](references/HOOKS_REFERENCE.md).

| Export | Returns | Purpose |
|--------|---------|---------|
| `useTrackEvent()` | `(eventName, data?) => Promise<void>` | Custom events |
| `useTempsAnalytics()` | `{ trackEvent, identify, trackPageview, enabled }` | **The context accessor** |
| `useTrackPageview()` | `() => void` | Manual pageviews |
| `usePageLeave(options?)` | `{ triggerPageLeave }` | Page-leave / time-on-page |
| `useEngagementTracking(options?)` | `{ engagementData, isTracking }` | Heartbeat engagement |
| `useSpeedAnalytics(options?)` | `void` | Web Vitals (TTFB, LCP, FID, FCP, CLS, INP) |
| `useScrollVisibility(options?)` | ref callback | Fires an event when the element scrolls into view |
| `useAnalytics(options)` | `{ track, identify }` | ⚠️ Standalone generic helper that **requires `{ client }`** — NOT the context accessor |

> ⚠️ The context accessor is **`useTempsAnalytics()`**, not `useAnalytics()`. `useAnalytics(options)` is a different, generic hook that throws without a `{ client }` argument. `reset()` and `getVisitorId()` do not exist.

### Track Custom Events

```tsx
'use client';
import { useTrackEvent } from '@temps-sdk/react-analytics';

function SubscribeButton() {
  const trackEvent = useTrackEvent();
  return (
    <button onClick={() => trackEvent('button_click', { button_id: 'subscribe', plan: 'premium' })}>
      Subscribe
    </button>
  );
}
```

### Identify Users — status: NOT YET FUNCTIONAL

`identify(userId, traits)` is exposed on the context (`useTempsAnalytics().identify`) **but the current SDK implements it as a no-op placeholder** ("implement when identity endpoint is available"). Do not tell the user identification works yet. Attach user attributes as `event_data` on `trackEvent` calls instead:

```tsx
'use client';
import { useTrackEvent } from '@temps-sdk/react-analytics';

const trackEvent = useTrackEvent();
trackEvent('signed_in', { user_id: user.id, plan: user.plan });
```

When the identity endpoint ships, switch to `useTempsAnalytics().identify(...)`.

## Session Recording

Session recording is configured **on the main provider** via `enableSessionRecording` + `sessionRecordingConfig`. See [SESSION_RECORDING.md](references/SESSION_RECORDING.md).

```tsx
<TempsAnalyticsProvider
  basePath="/api/_temps"
  enableSessionRecording={true}
  sessionRecordingConfig={{
    maskAllInputs: true,         // default true
    sessionSampleRate: 1.0,      // 0.0–1.0, default 1.0
    excludedPaths: ['/admin'],   // paths to never record
    blockClass: 'rr-block',      // default
    maskTextClass: 'rr-mask',    // default
    ignoreClass: 'rr-ignore',    // default
  }}
>
  {children}
</TempsAnalyticsProvider>
```

A separate `SessionRecordingProvider` + `useSessionRecordingControl` exist for user-toggleable recording (consent flows). Their **real** APIs (`defaultEnabled`/`persistPreference`, and `{ isEnabled, enable, disable, toggle }`) are documented in [SESSION_RECORDING.md](references/SESSION_RECORDING.md) — they are NOT `enabled`/`maskAllInputs`/`startRecording`.

## Verification Checklist

1. **On localhost:** with `ignoreLocalhost` default `true` you'll see nothing — expected. Temporarily set `ignoreLocalhost={false}` to verify wiring.
2. DevTools → Network: confirm POSTs to `/api/_temps/event` (and `/speed`, `/heartbeat`) on navigation and interaction.
3. Confirm responses are `2xx` (when Temps-hosted, the proxy accepts them from any host).
4. Check the Temps dashboard for incoming events / Web Vitals / session replays.
5. Run `npx tsc --noEmit` — it catches prop/hook drift immediately.
