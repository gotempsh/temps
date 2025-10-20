import { Button } from '@/components/ui/button'
import { HomeIcon } from 'lucide-react'
import { Link } from 'react-router-dom'

export default function NotFound() {
  return (
    <div className="flex min-h-screen flex-col items-center justify-center p-4 text-center">
      <div className="space-y-6">
        <div className="space-y-2">
          <h1 className="text-4xl font-bold tracking-tighter sm:text-5xl">
            404 - Page Not Found
          </h1>
          <p className="mx-auto max-w-[600px] text-gray-500 md:text-lg">
            The page you&apos;re looking for doesn&apos;t exist or has been
            moved.
          </p>
        </div>
        <div className="flex justify-center gap-4">
          <Button asChild>
            <Link to="/" className="flex items-center gap-2">
              <HomeIcon className="h-4 w-4" />
              Back to Dashboard
            </Link>
          </Button>
        </div>
      </div>
    </div>
  )
}
