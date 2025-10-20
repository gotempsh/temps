import { ImportWizard } from '@/components/imports/ImportWizard'
import { useNavigate } from 'react-router-dom'

export default function Import() {
  const navigate = useNavigate()

  return (
    <div className="container mx-auto py-8 px-4 max-w-5xl">
      <ImportWizard onCancel={() => navigate('/projects')} />
    </div>
  )
}
