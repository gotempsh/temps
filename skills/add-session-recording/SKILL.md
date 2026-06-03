---
name: add-session-recording
description: |
  Add privacy-aware session recording and replay to React applications using the Temps SDK. Captures user interactions for playback while respecting privacy through input masking, element blocking, and GDPR-compliant consent flows. Use when the user wants to: (1) Add session recording to their app, (2) Implement session replay functionality, (3) Record user sessions for debugging, (4) Add privacy-compliant screen recording, (5) Debug user issues with visual replay, (6) Implement rrweb-based recording, (7) Set up GDPR-compliant session capture. Triggers: "session recording", "session replay", "record sessions", "user replay", "screen recording", "rrweb", "session capture".
---

# Add Session Recording

Implement privacy-aware session recording with `@temps-sdk/react-analytics` (rrweb under the hood).

> **Verified against the real published package.** A prior version of this skill documented `<SessionRecordingProvider enabled maskAllInputs blockClass sampling>` and `startRecording`/`stopRecording`/`isRecording` — **none of those exist**. Confirm before changing:
> ```bash
> npm pack @temps-sdk/react-analytics@latest && tar -xzf temps-sdk-react-analytics-*.tgz \
>   && cat package/dist/types.d.ts package/dist/useSessionRecording.d.ts
> ```

## Installation

```bash
npm install @temps-sdk/react-analytics
```

## There are two ways to record — pick one

### A) Recommended: configure recording on the analytics provider

Recording is driven by the **main `TempsAnalyticsProvider`** via `enableSessionRecording` + `sessionRecordingConfig`. If the app already uses the analytics provider (see the `add-react-analytics` skill), just turn recording on — no second provider needed.

```tsx
// app/layout.tsx (Next App Router) — provider ships its own 'use client', layout stays a Server Component.
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <TempsAnalyticsProvider
          basePath="/api/_temps"
          enableSessionRecording={true}
          sessionRecordingConfig={{
            maskAllInputs: true,        // default true — mask password/sensitive inputs
            sessionSampleRate: 1.0,     // 0.0–1.0, default 1.0
            excludedPaths: ['/admin'],  // paths never recorded
            blockClass: 'rr-block',     // CSS class to block (default)
            maskTextClass: 'rr-mask',   // CSS class to mask text (default)
            ignoreClass: 'rr-ignore',   // CSS class to ignore (default)
          }}
        >
          {children}
        </TempsAnalyticsProvider>
      </body>
    </html>
  );
}
```

> `basePath="/api/_temps"` is correct for apps deployed on Temps — the proxy ingests `/api/_temps/session-replay` directly. See the `add-react-analytics` skill for the full basePath explanation.

### B) User-toggleable recording (consent flows)

For an explicit on/off toggle, use the separate `SessionRecordingProvider`. **Its real props are only `defaultEnabled` and `persistPreference`** — masking/blocking is still configured on the analytics provider's `sessionRecordingConfig`.

```ts
// Real signatures:
function SessionRecordingProvider(props: {
  children: React.ReactNode;
  defaultEnabled?: boolean;
  persistPreference?: boolean;   // remember the user's choice in localStorage
}): JSX.Element;

function useSessionRecordingControl(defaultEnabled?: boolean): {
  isEnabled: boolean;
  enable: () => void;
  disable: () => void;
  toggle: () => void;
};
```

```tsx
'use client';
import { SessionRecordingProvider, useSessionRecordingControl } from '@temps-sdk/react-analytics';

export function RecordingRoot({ children }: { children: React.ReactNode }) {
  return (
    <SessionRecordingProvider defaultEnabled={false} persistPreference={true}>
      {children}
    </SessionRecordingProvider>
  );
}

function RecordingControls() {
  const { isEnabled, toggle } = useSessionRecordingControl();
  return (
    <button onClick={toggle}>{isEnabled ? 'Stop' : 'Start'} Recording</button>
  );
}
```

> ⚠️ The control hook returns `{ isEnabled, enable, disable, toggle }` — **not** `{ isRecording, startRecording, stopRecording, toggleRecording }`. `useSessionRecording()` (no "Control") returns `{ isRecordingEnabled, enableRecording, disableRecording, toggleRecording, sessionId }` instead.

## Privacy controls

Masking/blocking uses the CSS classes configured in `sessionRecordingConfig` (defaults: `rr-block`, `rr-mask`, `rr-ignore`).

```tsx
// Block entirely (placeholder in replay)
<form className="rr-block">
  <input name="card" />
  <input name="cvv" />
</form>

// Mask text content (asterisks in replay)
<span className="rr-mask">{socialSecurityNumber}</span>

// Ignore from recording
<div className="rr-ignore"><NoisyWidget /></div>
```

> ⚠️ `data-rr-block` / `data-rr-mask` attribute selectors are **not** wired by default — use the configured CSS classes, or set custom selectors via `sessionRecordingConfig`.

## GDPR consent flow

```tsx
'use client';
import { useSessionRecordingControl } from '@temps-sdk/react-analytics';
import { useState, useEffect } from 'react';

function ConsentBanner() {
  const [show, setShow] = useState(false);
  const { enable, disable } = useSessionRecordingControl();

  useEffect(() => {
    const consent = localStorage.getItem('session_recording_consent');
    if (consent === null) setShow(true);
    else if (consent === 'true') enable();
  }, [enable]);

  if (!show) return null;
  return (
    <div className="fixed bottom-4 right-4 rounded bg-white p-4 shadow-lg">
      <p>We record sessions to improve your experience.</p>
      <div className="mt-2 flex gap-2">
        <button onClick={() => { localStorage.setItem('session_recording_consent', 'true'); enable(); setShow(false); }}>Accept</button>
        <button onClick={() => { localStorage.setItem('session_recording_consent', 'false'); disable(); setShow(false); }}>Decline</button>
      </div>
    </div>
  );
}
```

## Conditional recording

```tsx
// Only in production (via the analytics provider)
<TempsAnalyticsProvider basePath="/api/_temps" enableSessionRecording={process.env.NODE_ENV === 'production'}>

// Only for some users — gate the SessionRecordingProvider default
<SessionRecordingProvider defaultEnabled={user?.plan === 'enterprise'}>

// Exclude specific pages — use sessionRecordingConfig.excludedPaths instead of toggling per-route
sessionRecordingConfig={{ excludedPaths: ['/checkout', '/account/billing'] }}
```

## Verification

1. DevTools → Network: look for POSTs to `/api/_temps/session-replay`.
2. Interact with the app to generate events.
3. Open the session replay in the Temps dashboard.
4. Confirm masked/blocked elements are obscured in the replay.

> The ingest endpoint is `/api/_temps/session-replay` — **not** `/api/_temps/recordings`.
