import { useEffect, useState } from 'react'

interface ReloadableImageProps {
  src: string
  alt: string
  className?: string
  onLoad?: () => void
}

export function ReloadableImage({
  src,
  alt,
  className,
  onLoad,
}: ReloadableImageProps) {
  const [key, setKey] = useState(0)
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    let interval: NodeJS.Timer | undefined

    if (!loaded && src) {
      interval = setInterval(() => {
        setKey((prevKey) => prevKey + 1)
      }, 5000)
    }

    return () => {
      if (interval) clearInterval(interval)
    }
  }, [loaded, src])

  const handleLoad = () => {
    setLoaded(true)
    if (onLoad) onLoad()
  }

  const handleError = () => {
    setLoaded(false)
    setTimeout(() => {
      setKey((prevKey) => prevKey + 1)
    }, 1000)
  }

  return (
    <img
      key={key}
      src={`${src}?refresh=${key}`}
      alt={alt}
      className={className}
      onLoad={handleLoad}
      onError={handleError}
    />
  )
}
