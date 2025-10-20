import { Github } from 'lucide-react'
import { Link } from 'react-router-dom'
import { Badge } from '../ui/badge'

interface GitHubLinkProps {
  repo: string
  href: string
  className?: string
}

export default function GitHubLink({ repo, href, className }: GitHubLinkProps) {
  return (
    <Link to={href} target="_blank" className={`rounded-md text-lg`}>
      <Badge variant="secondary" className={`rounded-md ${className}`}>
        <div className="flex items-center gap-2">
          <Github className="h-4 w-4" />
          {repo}
        </div>
      </Badge>
    </Link>
  )
}
