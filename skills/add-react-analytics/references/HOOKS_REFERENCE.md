# Hooks API Reference

Complete reference for all Temps React Analytics hooks.

## useTrackEvent

Track custom events with arbitrary properties.

```tsx
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

  return (
    <button onClick={() => handleAddToCart('prod_123', 29.99)}>
      Add to Cart
    </button>
  );
}
```

## useAnalytics

Access the analytics context for user identification and core analytics functions.

```tsx
'use client';
import { useAnalytics } from '@temps-sdk/react-analytics';

function App() {
  const { identify, reset, getVisitorId } = useAnalytics();

  // Identify a user
  identify('user_123', {
    email: 'user@example.com',
    name: 'John Doe',
    plan: 'premium'
  });

  // Reset analytics (on logout)
  reset();

  // Get anonymous visitor ID
  const visitorId = getVisitorId();
}
```

## useScrollVisibility

Track when elements scroll into view using Intersection Observer.

```tsx
'use client';
import { useScrollVisibility } from '@temps-sdk/react-analytics';

function ProductCard({ product }) {
  const scrollRef = useScrollVisibility('product_viewed', {
    product_id: product.id,
    product_name: product.name,
    price: product.price,
  }, {
    threshold: 0.5,    // Trigger when 50% visible
    once: true,        // Track only once
    enabled: true,     // Enable tracking
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
    threshold: [0, 0.25, 0.5, 0.75, 1.0],
    once: false,
  });

  return <section ref={heroRef}>Welcome</section>;
}
```

**Options:**
| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `threshold` | `number \| number[]` | `0` | Visibility percentage (0-1) |
| `once` | `boolean` | `true` | Track only on first visibility |
| `enabled` | `boolean` | `true` | Enable/disable tracking |
| `root` | `Element` | viewport | Viewport reference |
| `rootMargin` | `string` | `'0px'` | Margin around root |

## usePageLeave

Track when users leave pages and time spent.

```tsx
'use client';
import { usePageLeave } from '@temps-sdk/react-analytics';

function ArticlePage({ articleId }) {
  usePageLeave((data) => {
    console.log('User spent', data.timeOnPage, 'ms on article', articleId);
    // Data automatically sent to analytics
  });

  return <article>Content</article>;
}
```

**Callback data:**
- `timeOnPage`: Time in milliseconds
- `page`: Current URL
- `timestamp`: ISO timestamp

**Features:**
- Uses `sendBeacon` for reliable transmission
- Handles page refresh, tab close, navigation
- No configuration needed

## useEngagementTracking

Monitor active engagement with heartbeat tracking.

```tsx
'use client';
import { useEngagementTracking } from '@temps-sdk/react-analytics';

function VideoPage() {
  useEngagementTracking({
    heartbeatInterval: 5000,      // Heartbeat every 5 seconds
    engagementThreshold: 2000,    // Engaged after 2 seconds
    inactivityTimeout: 30000,     // Inactive after 30 seconds
    onEngagementUpdate: (data) => {
      console.log('Engaged for', data.engagementTime, 'ms');
    },
    onPageLeave: (data) => {
      console.log('Final engagement:', data.totalEngagementTime, 'ms');
    },
  });

  return <video src="/video.mp4" controls />;
}
```

**Options:**
| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `heartbeatInterval` | `number` | `30000` | Ms between pings |
| `engagementThreshold` | `number` | `5000` | Ms before considered engaged |
| `inactivityTimeout` | `number` | `60000` | Ms of inactivity to pause |
| `onEngagementUpdate` | `function` | - | Callback on each heartbeat |
| `onPageLeave` | `function` | - | Callback when leaving |

**Tracked events:** Mouse movement, keyboard input, scroll, touch, click

## useSpeedAnalytics

Track Core Web Vitals and performance metrics.

```tsx
'use client';
import { useSpeedAnalytics } from '@temps-sdk/react-analytics';

function App({ children }) {
  useSpeedAnalytics({
    onMetric: (metric) => {
      console.log(metric.name, metric.value);
    },
  });

  return <div>{children}</div>;
}
```

**Tracked metrics:**
| Metric | Description |
|--------|-------------|
| TTFB | Time to First Byte |
| LCP | Largest Contentful Paint |
| FID | First Input Delay |
| FCP | First Contentful Paint |
| CLS | Cumulative Layout Shift |
| INP | Interaction to Next Paint |

## useTrackPageview

Manual page view tracking for custom routing.

```tsx
'use client';
import { useTrackPageview } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';
import { usePathname } from 'next/navigation';

function PageViewTracker() {
  const pathname = usePathname();
  const trackPageview = useTrackPageview();

  useEffect(() => {
    trackPageview();
  }, [pathname, trackPageview]);

  return null;
}
```

## useSessionRecording / useSessionRecordingControl

See [SESSION_RECORDING.md](SESSION_RECORDING.md) for complete session recording documentation.
