---
name: add-session-recording
description: |
  Add privacy-aware session recording and replay to React applications using Temps SDK. Captures user interactions for playback while respecting privacy through input masking, element blocking, and GDPR-compliant consent flows. Use when the user wants to: (1) Add session recording to their app, (2) Implement session replay functionality, (3) Record user sessions for debugging, (4) Add privacy-compliant screen recording, (5) Debug user issues with visual replay, (6) Implement rrweb-based recording, (7) Set up GDPR-compliant session capture. Triggers: "session recording", "session replay", "record sessions", "user replay", "screen recording", "rrweb", "session capture".
---

# Add Session Recording

Privacy-aware session recording with Temps SDK (rrweb under the hood).

## Installation

```bash
npm install @temps-sdk/react-analytics
```

**Validate**: Confirm the package resolves — run `npm ls @temps-sdk/react-analytics` and check for no peer dependency warnings.

## Quick Setup

```tsx
// app/providers.tsx or app/layout.tsx
'use client';

import {
  TempsAnalyticsProvider,
  SessionRecordingProvider
} from '@temps-sdk/react-analytics';

export function Providers({ children }) {
  return (
    <TempsAnalyticsProvider basePath="/api/_temps">
      <SessionRecordingProvider
        enabled={true}
        maskAllInputs={true}
        blockClass="sensitive"
      >
        {children}
      </SessionRecordingProvider>
    </TempsAnalyticsProvider>
  );
}
```

## Provider Options

```tsx
<SessionRecordingProvider
  enabled={true}              // Enable recording
  maskAllInputs={true}        // Mask all input values (recommended)
  maskAllText={false}         // Mask all text content
  blockClass="sensitive"      // CSS class to block elements
  ignoreClass="no-record"     // CSS class to ignore elements
  sampling={{
    mousemove: true,
    mouseInteraction: true,
    scroll: true,
    input: 'last',            // 'all' | 'last' | false
  }}
>
  {children}
</SessionRecordingProvider>
```

**Validate**: After adding the providers, open DevTools Network tab and look for outgoing requests to `/api/_temps/recordings`. If no requests appear, verify `enabled={true}` and that `TempsAnalyticsProvider` wraps `SessionRecordingProvider`.

## Control Recording Programmatically

```tsx
'use client';

import { useSessionRecordingControl } from '@temps-sdk/react-analytics';

function RecordingControls() {
  const {
    isRecording,
    startRecording,
    stopRecording,
    toggleRecording
  } = useSessionRecordingControl();

  return (
    <div>
      <span>Recording: {isRecording ? 'Active' : 'Paused'}</span>
      <button onClick={toggleRecording}>
        {isRecording ? 'Stop' : 'Start'} Recording
      </button>
    </div>
  );
}
```

## Privacy Controls

Three methods to protect sensitive content:

```tsx
// CSS class (configured via blockClass in provider)
<div className="sensitive">
  <CreditCardForm />
</div>

// Data attribute — block entirely
<input type="password" data-rr-block />
<section data-rr-block><MedicalRecords /></section>

// Data attribute — mask (shows asterisks in replay)
<span data-rr-mask>{socialSecurityNumber}</span>
<input name="dob" data-rr-mask />
```

**Validate**: After adding privacy attributes, record a test session and replay it in the Temps dashboard. Confirm that blocked elements appear as placeholders and masked fields show asterisks.

## GDPR Consent Flow

```tsx
'use client';

import { useSessionRecordingControl } from '@temps-sdk/react-analytics';
import { useState, useEffect } from 'react';

function ConsentBanner() {
  const [showBanner, setShowBanner] = useState(false);
  const { startRecording, stopRecording } = useSessionRecordingControl();

  useEffect(() => {
    const consent = localStorage.getItem('session_recording_consent');
    if (consent === null) {
      setShowBanner(true);
    } else if (consent === 'true') {
      startRecording();
    }
  }, []);

  const handleAccept = () => {
    localStorage.setItem('session_recording_consent', 'true');
    startRecording();
    setShowBanner(false);
  };

  const handleDecline = () => {
    localStorage.setItem('session_recording_consent', 'false');
    stopRecording();
    setShowBanner(false);
  };

  if (!showBanner) return null;

  return (
    <div className="fixed bottom-4 right-4 p-4 bg-white shadow-lg rounded">
      <p>We record sessions to improve your experience.</p>
      <div className="flex gap-2 mt-2">
        <button onClick={handleAccept}>Accept</button>
        <button onClick={handleDecline}>Decline</button>
      </div>
    </div>
  );
}
```

## Conditional Recording

```tsx
// Only record in production
<SessionRecordingProvider
  enabled={process.env.NODE_ENV === 'production'}
>

// Only record for specific users
<SessionRecordingProvider
  enabled={user?.plan === 'enterprise'}
>

// Disable for specific pages
function CheckoutPage() {
  const { stopRecording, startRecording } = useSessionRecordingControl();

  useEffect(() => {
    stopRecording();
    return () => startRecording();
  }, []);

  return <CheckoutForm />;
}
```

## Final Verification

1. Open DevTools Network tab — confirm requests to `/api/_temps/recordings`
2. Interact with the app to generate recording data
3. Check Temps dashboard for session replays
4. Replay a session and verify sensitive data is masked/blocked
5. Test the GDPR consent flow: decline consent, then confirm no recording requests are sent
