import { Button } from '@/components/ui/button'
import { Download } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

export function ImportServiceButton({ onSuccess }: { onSuccess?: () => void }) {
  const navigate = useNavigate()

  return (
    <Button
      variant="outline"
      className="gap-2"
      onClick={() => navigate('/storage/import')}
    >
      <Download className="h-4 w-4" />
      Import Service
    </Button>
  )
}
