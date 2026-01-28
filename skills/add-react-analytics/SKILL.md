---
name: add-react-analytics
description: |
  Add Temps analytics to React applications with comprehensive tracking capabilities including page views, custom events, scroll tracking, engagement monitoring, session recording, and Web Vitals performance metrics. Use when the user wants to: (1) Add analytics to a React app (Next.js App Router, Next.js Pages Router, Vite, Create React App, or Remix), (2) Track user events or interactions, (3) Monitor scroll depth or element visibility, (4) Add session recording/replay, (5) Track Web Vitals or performance metrics, (6) Measure user engagement or time on page, (7) Identify users for analytics, (8) Set up product analytics or telemetry. Triggers: "add analytics", "track events", "session recording", "web vitals", "user tracking", "temps analytics", "react analytics".
---

# Add React Analytics

Integrate Temps analytics SDK into React applications with full tracking capabilities.

## Installation

```bash
npm install @temps-sdk/react-analytics
# or: yarn add / pnpm add / bun add
```

## Framework Setup

Detect the user's framework and apply the appropriate setup:

### Next.js App Router (13+)

```tsx
// app/layout.tsx
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
// src/main.tsx or src/index.tsx
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

## Provider Configuration

```tsx
<TempsAnalyticsProvider
  basePath="/api/_temps"
  autoTrack={{
    pageviews: true,       // Auto-track page views
    pageLeave: true,       // Track time on page
    speedAnalytics: true,  // Track Web Vitals
    engagement: true,      // Track engagement
    engagementInterval: 30000,
  }}
  debug={process.env.NODE_ENV === 'development'}
>
  {children}
</TempsAnalyticsProvider>
```

## Available Hooks

For detailed API reference, see [HOOKS_REFERENCE.md](references/HOOKS_REFERENCE.md).

### Quick Reference

| Hook | Purpose |
|------|---------|
| `useTrackEvent` | Track custom events |
| `useAnalytics` | Access analytics context, identify users |
| `useScrollVisibility` | Track element visibility on scroll |
| `usePageLeave` | Track page leave and time on page |
| `useEngagementTracking` | Heartbeat engagement monitoring |
| `useSpeedAnalytics` | Web Vitals (LCP, FCP, CLS, TTFB, INP) |
| `useTrackPageview` | Manual page view tracking |

### Track Custom Events

```tsx
'use client';
import { useTrackEvent } from '@temps-sdk/react-analytics';

function MyComponent() {
  const trackEvent = useTrackEvent();

  const handleClick = () => {
    trackEvent('button_click', {
      button_id: 'subscribe',
      plan: 'premium'
    });
  };

  return <button onClick={handleClick}>Subscribe</button>;
}
```

### Identify Users

```tsx
'use client';
import { useAnalytics } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';

function UserProfile({ user }) {
  const { identify } = useAnalytics();

  useEffect(() => {
    if (user) {
      identify(user.id, {
        email: user.email,
        name: user.name,
        plan: user.subscription?.plan
      });
    }
  }, [user, identify]);

  return <div>Profile</div>;
}
```

## Session Recording

For privacy-aware session replay, see [SESSION_RECORDING.md](references/SESSION_RECORDING.md).

```tsx
import { SessionRecordingProvider } from '@temps-sdk/react-analytics';

<SessionRecordingProvider
  enabled={true}
  maskAllInputs={true}
  blockClass="sensitive"
>
  {children}
</SessionRecordingProvider>
```

## Verification Checklist

After implementation:
1. Check browser DevTools Network tab for `/api/_temps` requests
2. Verify events appear in Temps dashboard
3. Test session recording playback
4. Confirm Web Vitals are being captured
