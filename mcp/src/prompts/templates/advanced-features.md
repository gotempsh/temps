## Additional Features

### Automatic Page Views
The SDK automatically tracks page views in your application. This happens automatically when you add the provider - no additional configuration needed!

## Advanced Tracking Features

### Track Custom Events

Use the `useTrackEvent` hook to track any custom events in your application:

```typescript
'use client';

import { useTrackEvent } from '@temps-sdk/react-analytics';

function ProductPage() {
  const trackEvent = useTrackEvent();

  const handleAddToCart = (productId: string, price: number) => {
    trackEvent('add_to_cart', {
      product_id: productId,
      price: price,
      currency: 'USD',
      timestamp: new Date().toISOString(),
    });
  };

  const handleWishlist = (productId: string) => {
    trackEvent('add_to_wishlist', {
      product_id: productId,
    });
  };

  return (
    <div>
      <button onClick={() => handleAddToCart('prod_123', 29.99)}>
        Add to Cart
      </button>
      <button onClick={() => handleWishlist('prod_123')}>
        Add to Wishlist
      </button>
    </div>
  );
}
```

### Track Scroll Visibility

Automatically track when elements scroll into view using the `useScrollVisibility` hook:

```typescript
'use client';

import { useScrollVisibility } from '@temps-sdk/react-analytics';

function ProductCard({ product }) {
  const scrollRef = useScrollVisibility('product_viewed', {
    product_id: product.id,
    product_name: product.name,
    price: product.price,
  }, {
    threshold: 0.5,        // Trigger when 50% visible
    once: true,            // Track only once
    enabled: true,         // Enable tracking
  });

  return (
    <div ref={scrollRef} className="product-card">
      <h3>{product.name}</h3>
      <p>${product.price}</p>
    </div>
  );
}

// Track multiple visibility thresholds
function HeroSection() {
  const heroRef = useScrollVisibility('hero_visible', {
    section: 'hero',
  }, {
    threshold: [0, 0.25, 0.5, 0.75, 1.0], // Track at each 25% increment
    once: false,                           // Track multiple times
  });

  return (
    <section ref={heroRef}>
      <h1>Welcome to our site</h1>
    </section>
  );
}
```

**Options:**
- `threshold`: Number or array (0-1) for visibility percentage
- `once`: Boolean - track only on first visibility
- `enabled`: Boolean - conditionally enable tracking
- `root`: Element - viewport reference (default: browser viewport)
- `rootMargin`: String - margin around root (e.g., '0px 0px -100px 0px')

### Track Page Leave & Time on Page

Track when users leave pages and how long they spent:

```typescript
'use client';

import { usePageLeave } from '@temps-sdk/react-analytics';

function ArticlePage({ articleId }) {
  usePageLeave((data) => {
    console.log('User spent', data.timeOnPage, 'ms on article', articleId);
    // Data is automatically sent to analytics
  });

  return (
    <article>
      <h1>Article Content</h1>
      <p>Your article text...</p>
    </article>
  );
}

// With custom data
function CheckoutPage() {
  const [cartValue, setCartValue] = useState(0);

  usePageLeave((data) => {
    // Automatically sends:
    // - timeOnPage (milliseconds)
    // - page URL
    // - timestamp
    console.log('User left checkout with cart value:', cartValue);
  });

  return <div>Checkout content</div>;
}
```

**Features:**
- Automatically tracks time spent on page
- Uses `sendBeacon` for reliable data transmission even when navigating away
- Handles page refresh, closing tab, and navigation
- No configuration needed - just use the hook

### Track User Engagement

Monitor active engagement time with heartbeat tracking:

```typescript
'use client';

import { useEngagementTracking } from '@temps-sdk/react-analytics';

function VideoPage() {
  useEngagementTracking({
    heartbeatInterval: 5000,      // Send heartbeat every 5 seconds
    engagementThreshold: 2000,    // Consider engaged after 2 seconds
    inactivityTimeout: 30000,     // Consider inactive after 30 seconds
    onEngagementUpdate: (data) => {
      console.log('User engaged for', data.engagementTime, 'ms');
      console.log('Active:', data.isActive);
    },
    onPageLeave: (data) => {
      console.log('Final engagement:', data.totalEngagementTime, 'ms');
    },
  });

  return (
    <div>
      <video src="/video.mp4" controls />
    </div>
  );
}

// Minimal setup with defaults
function BlogPost() {
  useEngagementTracking({
    heartbeatInterval: 10000, // Every 10 seconds
  });

  return <article>Blog content</article>;
}
```

**Options:**
- `heartbeatInterval`: Milliseconds between engagement pings (default: 30000)
- `engagementThreshold`: Milliseconds before considering user engaged (default: 5000)
- `inactivityTimeout`: Milliseconds of inactivity before pausing tracking (default: 60000)
- `onEngagementUpdate`: Callback fired on each heartbeat with engagement data
- `onPageLeave`: Callback fired when user leaves with final statistics

**Tracked Events:**
- Mouse movement
- Keyboard input
- Scroll events
- Touch events
- Click events

### Session Recording

Capture user sessions with visual replay functionality:

```typescript
// app/layout.tsx or app/providers.tsx
'use client';

import { SessionRecordingProvider } from '@temps-sdk/react-analytics';

export function Providers({ children }) {
  return (
    <SessionRecordingProvider
      enabled={true}
      maskAllInputs={true}      // Mask password/sensitive inputs
      maskAllText={false}        // Don't mask all text content
      blockClass="sensitive"     // CSS class to block from recording
      ignoreClass="ignore-recording" // CSS class to ignore
    >
      {children}
    </SessionRecordingProvider>
  );
}

// In any component - control recording
'use client';

import { useSessionRecordingControl } from '@temps-sdk/react-analytics';

function RecordingControls() {
  const { isRecording, startRecording, stopRecording, toggleRecording } =
    useSessionRecordingControl();

  return (
    <div>
      <p>Recording: {isRecording ? 'ON' : 'OFF'}</p>
      <button onClick={startRecording}>Start</button>
      <button onClick={stopRecording}>Stop</button>
      <button onClick={toggleRecording}>Toggle</button>
    </div>
  );
}

// Block sensitive content from recording
function PaymentForm() {
  return (
    <form>
      <input
        type="text"
        name="cardNumber"
        className="sensitive" // Blocked from recording
      />
      <input
        type="text"
        name="cvv"
        data-rr-block // Alternative: use data attribute
      />
      <button type="submit">Pay</button>
    </form>
  );
}
```

**Privacy Options:**
- `maskAllInputs`: Boolean - mask all input fields (recommended for privacy)
- `maskAllText`: Boolean - mask all text content
- `blockClass`: String - CSS class to completely block elements
- `ignoreClass`: String - CSS class to ignore elements
- Use `data-rr-block` attribute to block specific elements

### Performance Tracking (Web Vitals)

Automatically track Core Web Vitals and performance metrics:

```typescript
'use client';

import { useSpeedAnalytics } from '@temps-sdk/react-analytics';

function MyApp({ children }) {
  useSpeedAnalytics({
    onMetric: (metric) => {
      console.log(metric.name, metric.value);
    },
  });

  return <div>{children}</div>;
}

// Tracked metrics:
// - TTFB: Time to First Byte
// - LCP: Largest Contentful Paint
// - FID: First Input Delay
// - FCP: First Contentful Paint
// - CLS: Cumulative Layout Shift
// - INP: Interaction to Next Paint
```

All metrics are automatically sent to Temps Analytics for monitoring and analysis.

### Manual Page View Tracking

For single-page applications with custom routing:

```typescript
'use client';

import { useTrackPageview } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';
import { usePathname } from 'next/navigation';

function PageViewTracker() {
  const pathname = usePathname();
  const trackPageview = useTrackPageview();

  useEffect(() => {
    // Track pageview on route change
    trackPageview();
  }, [pathname, trackPageview]);

  return null;
}
```

## Provider Configuration

Customize the analytics provider with additional options:

```typescript
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function RootLayout({ children }) {
  return (
    <html>
      <body>
        <TempsAnalyticsProvider
          basePath="/api/_temps"
          autoTrack={{
            pageviews: true,      // Auto-track page views
            pageLeave: true,      // Auto-track page leave events
            speedAnalytics: true, // Auto-track Web Vitals
            engagement: true,     // Auto-track engagement
            engagementInterval: 30000, // Heartbeat interval (ms)
          }}
          debug={process.env.NODE_ENV === 'development'} // Enable debug logs
        >
          {children}
        </TempsAnalyticsProvider>
      </body>
    </html>
  );
}
```

## Troubleshooting

**Analytics not appearing?**
- Check browser console for errors
- Verify the `basePath` is correct (`/api/_temps`)
- Ensure your project ID is valid in Temps dashboard
- Check network tab to see if events are being sent
- Enable debug mode in development: `debug={true}`

**Session recording not working?**
- Verify `SessionRecordingProvider` wraps your app
- Check browser console for rrweb errors
- Ensure sensitive inputs have proper masking
- Test with `useSessionRecordingControl` hook

**Events not tracking?**
- Verify component is client-side (`'use client'` directive)
- Check that `TempsAnalyticsProvider` is properly configured
- Ensure hooks are called inside components (not conditionally)
- Use browser DevTools Network tab to inspect outgoing requests

**Performance impact concerns?**
- Session recording: ~2-3% CPU overhead (configurable sampling)
- Engagement tracking: Minimal overhead with heartbeat system
- Scroll tracking: Uses efficient Intersection Observer API
- All tracking uses `requestIdleCallback` when available

**Need help?**
- View full documentation at Temps docs
- Check example implementations in the SDK repository
- Review source code: `@temps-sdk/react-analytics` package
- Contact support if issues persist

Your analytics integration is ready! 🎉
