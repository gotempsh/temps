import { BackupDetail } from '@/pages/BackupDetail'
import { S3SourceDetail } from '@/pages/S3SourceDetail'

const routes = [
  {
    path: '/backups/s3-sources/:id',
    element: <S3SourceDetail />,
  },
  {
    path: '/backups/s3-sources/:id/backups/:backupId',
    element: <BackupDetail />,
  },
]
