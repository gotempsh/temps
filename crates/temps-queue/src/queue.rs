use std::sync::Arc;

use temps_core::async_trait::async_trait;
use temps_core::{
    CalculateRepositoryPresetJob, Job, JobQueue, JobReceiver, ProvisionCertificateJob, QueueError,
    RenewCertificateJob, UpdateRepoFrameworkJob,
};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info};

#[derive(Error, Debug)]
pub enum QueueServiceError {
    #[error("Failed to send job to queue: {details}")]
    QueueSendError { details: String, job_type: String },

    #[error("Queue channel closed")]
    QueueChannelClosed { job_type: String },

    #[error("Invalid job data: {details}")]
    InvalidJobData { details: String, job_type: String },

    #[error("Queue service error: {0}")]
    Internal(String),
}

impl<T> From<mpsc::error::SendError<T>> for QueueServiceError {
    fn from(_err: mpsc::error::SendError<T>) -> Self {
        QueueServiceError::QueueChannelClosed {
            job_type: "unknown".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct QueueService {
    job_sender: mpsc::Sender<Job>,
}

#[derive(Clone)]
pub struct BroadcastQueueService {
    broadcast_sender: broadcast::Sender<Job>,
}

// Wrapper for broadcast::Receiver to implement JobReceiver trait
pub struct BroadcastJobReceiver {
    receiver: broadcast::Receiver<Job>,
}

#[async_trait]
impl JobReceiver for BroadcastJobReceiver {
    async fn recv(&mut self) -> Result<Job, QueueError> {
        debug!("🎧 JobReceiver::recv - Waiting for job...");

        let result = self.receiver.recv().await.map_err(|e| match e {
            broadcast::error::RecvError::Closed => {
                error!("❌ Broadcast channel closed");
                QueueError::ChannelClosed
            }
            broadcast::error::RecvError::Lagged(n) => {
                error!("⚠️ Receiver lagged by {} messages", n);
                QueueError::ReceiveError(format!("Receiver lagged by {} messages", n))
            }
        });

        match &result {
            Ok(job) => {
                debug!("📨 Successfully received job: {}", job);
                debug!("🎯 Job type received: {}", std::any::type_name_of_val(job));
            }
            Err(e) => {
                error!("💥 Failed to receive job: {}", e);
            }
        }

        result
    }
}

#[async_trait]
impl JobQueue for BroadcastQueueService {
    async fn send(&self, job: Job) -> Result<(), QueueError> {
        debug!("🚀 JobQueue::send - Broadcasting job: {}", job);
        let subscriber_count = self.broadcast_sender.receiver_count();
        debug!(
            "📊 Broadcast channel info - subscriber count: {}",
            subscriber_count
        );

        // Critical issue detection: no subscribers
        if subscriber_count == 0 {
            error!(
                "🚨 CRITICAL: No subscribers listening to broadcast channel! Job will be lost: {}",
                job
            );
            error!("🔍 This means the job processor may not be running or not subscribed");
        }

        let result = self.broadcast_sender.send(job.clone()).map_err(|e| {
            error!("❌ Failed to broadcast job {}: {}", job, e);
            QueueError::SendError(format!("Broadcast send failed: {}", e))
        });

        match &result {
            Ok(_) => {
                debug!("✅ Successfully broadcasted job: {}", job);
                debug!("📈 Job sent to {} subscribers", subscriber_count);
            }
            Err(e) => {
                error!("💥 Broadcast send error: {}", e);
            }
        }

        result?;
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn JobReceiver> {
        debug!("📡 JobQueue::subscribe - Creating new subscriber");
        debug!(
            "📊 Current subscriber count before: {}",
            self.broadcast_sender.receiver_count()
        );

        let receiver = BroadcastJobReceiver {
            receiver: self.broadcast_sender.subscribe(),
        };

        debug!(
            "📊 Current subscriber count after: {}",
            self.broadcast_sender.receiver_count()
        );
        debug!("✅ New subscriber created successfully");

        Box::new(receiver)
    }
}

impl QueueService {
    pub fn new(job_sender: mpsc::Sender<Job>) -> Self {
        Self { job_sender }
    }

    pub fn create_channel(buffer_size: usize) -> (QueueService, mpsc::Receiver<Job>) {
        let (sender, receiver) = mpsc::channel(buffer_size);
        (QueueService::new(sender), receiver)
    }
}

impl BroadcastQueueService {
    pub fn new(broadcast_sender: broadcast::Sender<Job>) -> Self {
        debug!("🏗️ Creating new BroadcastQueueService");
        debug!(
            "📊 Initial broadcast sender capacity: {}",
            broadcast_sender.receiver_count()
        );
        Self { broadcast_sender }
    }

    pub fn create_broadcast_channel(
        buffer_size: usize,
    ) -> (BroadcastQueueService, broadcast::Receiver<Job>) {
        debug!(
            "🔧 Creating broadcast channel with buffer size: {}",
            buffer_size
        );
        let (sender, receiver) = broadcast::channel(buffer_size);
        debug!("✅ Broadcast channel created successfully");
        (BroadcastQueueService::new(sender), receiver)
    }

    /// Create a new broadcast queue that implements the JobQueue trait
    /// Returns (queue, keep_alive_receiver) - the receiver must be kept alive!
    pub fn create_job_queue_with_receiver(
        buffer_size: usize,
    ) -> (Box<dyn JobQueue>, broadcast::Receiver<Job>) {
        debug!(
            "📦 Creating boxed JobQueue with buffer size: {}",
            buffer_size
        );
        let (sender, receiver) = broadcast::channel(buffer_size);
        debug!("🔧 Returning receiver to keep channel alive");
        debug!("✅ Boxed JobQueue created");
        (Box::new(BroadcastQueueService::new(sender)), receiver)
    }

    /// Create a new broadcast queue that implements the JobQueue trait
    /// Returns (queue, keep_alive_receiver) - the receiver must be kept alive!
    pub fn create_job_queue_arc_with_receiver(
        buffer_size: usize,
    ) -> (Arc<dyn JobQueue>, broadcast::Receiver<Job>) {
        debug!("🔄 Creating Arc JobQueue with buffer size: {}", buffer_size);
        let (sender, receiver) = broadcast::channel(buffer_size);
        debug!("🔧 Returning receiver to keep channel alive");
        debug!("✅ Arc JobQueue created");
        (Arc::new(BroadcastQueueService::new(sender)), receiver)
    }

    /// DEPRECATED: Use create_job_queue_with_receiver instead
    /// This method drops the receiver immediately, causing the channel to close
    pub fn create_job_queue(buffer_size: usize) -> Box<dyn JobQueue> {
        debug!(
            "📦 Creating boxed JobQueue with buffer size: {}",
            buffer_size
        );
        let (sender, _receiver) = broadcast::channel(buffer_size);
        debug!("⚠️ WARNING: Receiver will be dropped, channel may close immediately!");
        debug!("✅ Boxed JobQueue created (but may not work!)");
        Box::new(BroadcastQueueService::new(sender))
    }

    /// DEPRECATED: Use create_job_queue_arc_with_receiver instead
    /// This method drops the receiver immediately, causing the channel to close
    pub fn create_job_queue_arc(buffer_size: usize) -> Arc<dyn JobQueue> {
        debug!("🔄 Creating Arc JobQueue with buffer size: {}", buffer_size);
        let (sender, _receiver) = broadcast::channel(buffer_size);
        debug!("⚠️ WARNING: Receiver will be dropped, channel may close immediately!");
        debug!("✅ Arc JobQueue created (but may not work!)");
        Arc::new(BroadcastQueueService::new(sender))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Job> {
        debug!("📡 Creating direct broadcast subscription");
        debug!(
            "📊 Current subscriber count before direct subscribe: {}",
            self.broadcast_sender.receiver_count()
        );
        let receiver = self.broadcast_sender.subscribe();
        debug!(
            "📊 Current subscriber count after direct subscribe: {}",
            self.broadcast_sender.receiver_count()
        );
        debug!("✅ Direct broadcast subscription created");
        receiver
    }

    pub async fn launch_repo_framework_update(
        &self,
        data: UpdateRepoFrameworkJob,
    ) -> Result<(), QueueServiceError> {
        info!("Broadcasting repo framework update job");
        self.broadcast_sender
            .send(Job::UpdateRepoFramework(data))
            .map_err(|e| {
                error!("Failed to broadcast repo framework update job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "repo_framework_update".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_certificate_provision(
        &self,
        data: ProvisionCertificateJob,
    ) -> Result<(), QueueServiceError> {
        info!(
            "Broadcasting certificate provisioning job for domain: {}",
            data.domain
        );
        self.broadcast_sender
            .send(Job::ProvisionCertificate(data))
            .map_err(|e| {
                error!("Failed to broadcast certificate provision job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "certificate_provision".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_certificate_renewal(
        &self,
        data: RenewCertificateJob,
    ) -> Result<(), QueueServiceError> {
        info!(
            "Broadcasting certificate renewal job for domain: {}",
            data.domain
        );
        self.broadcast_sender
            .send(Job::RenewCertificate(data))
            .map_err(|e| {
                error!("Failed to broadcast certificate renewal job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "certificate_renewal".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_domain_added(&self, data: String) -> Result<(), QueueServiceError> {
        info!("Broadcasting custom domain added job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Domain cannot be empty".to_string(),
                job_type: "custom_domain_added".to_string(),
            });
        }
        self.broadcast_sender
            .send(Job::CustomDomainAdded(data))
            .map_err(|e| {
                error!("Failed to broadcast custom domain added job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_domain_added".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_domain_removed(
        &self,
        data: String,
    ) -> Result<(), QueueServiceError> {
        info!("Broadcasting custom domain removed job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Domain cannot be empty".to_string(),
                job_type: "custom_domain_removed".to_string(),
            });
        }
        self.broadcast_sender
            .send(Job::CustomDomainRemoved(data))
            .map_err(|e| {
                error!("Failed to broadcast custom domain removed job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_domain_removed".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_route_added(&self, data: String) -> Result<(), QueueServiceError> {
        info!("Broadcasting custom route added job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Route data cannot be empty".to_string(),
                job_type: "custom_route_added".to_string(),
            });
        }
        self.broadcast_sender
            .send(Job::CustomRouteAdded(data))
            .map_err(|e| {
                error!("Failed to broadcast custom route added job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_route_added".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_route_removed(&self, data: String) -> Result<(), QueueServiceError> {
        info!("Broadcasting custom route removed job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Route data cannot be empty".to_string(),
                job_type: "custom_route_removed".to_string(),
            });
        }
        self.broadcast_sender
            .send(Job::CustomRouteRemoved(data))
            .map_err(|e| {
                error!("Failed to broadcast custom route removed job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_route_removed".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn queue_preset_calculation(
        &self,
        repository_id: i32,
    ) -> Result<(), QueueServiceError> {
        info!(
            "Broadcasting preset calculation job for repository: {}",
            repository_id
        );
        let job_data = CalculateRepositoryPresetJob { repository_id };
        self.broadcast_sender
            .send(Job::CalculateRepositoryPreset(job_data))
            .map_err(|e| {
                error!("Failed to broadcast preset calculation job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "calculate_repository_preset".to_string(),
                }
            })?;
        Ok(())
    }
}

impl QueueService {
    pub async fn launch_repo_framework_update(
        &self,
        data: UpdateRepoFrameworkJob,
    ) -> Result<(), QueueServiceError> {
        info!("Queueing repo framework update job");
        self.job_sender
            .send(Job::UpdateRepoFramework(data))
            .await
            .map_err(|e| {
                error!("Failed to queue repo framework update job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "repo_framework_update".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_certificate_provision(
        &self,
        data: ProvisionCertificateJob,
    ) -> Result<(), QueueServiceError> {
        info!(
            "Queueing certificate provisioning job for domain: {}",
            data.domain
        );
        self.job_sender
            .send(Job::ProvisionCertificate(data))
            .await
            .map_err(|e| {
                error!("Failed to queue certificate provision job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "certificate_provision".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_certificate_renewal(
        &self,
        data: RenewCertificateJob,
    ) -> Result<(), QueueServiceError> {
        info!(
            "Queueing certificate renewal job for domain: {}",
            data.domain
        );
        self.job_sender
            .send(Job::RenewCertificate(data))
            .await
            .map_err(|e| {
                error!("Failed to queue certificate renewal job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "certificate_renewal".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_domain_added(&self, data: String) -> Result<(), QueueServiceError> {
        info!("Queueing custom domain added job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Domain cannot be empty".to_string(),
                job_type: "custom_domain_added".to_string(),
            });
        }
        self.job_sender
            .send(Job::CustomDomainAdded(data))
            .await
            .map_err(|e| {
                error!("Failed to queue custom domain added job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_domain_added".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_domain_removed(
        &self,
        data: String,
    ) -> Result<(), QueueServiceError> {
        info!("Queueing custom domain removed job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Domain cannot be empty".to_string(),
                job_type: "custom_domain_removed".to_string(),
            });
        }
        self.job_sender
            .send(Job::CustomDomainRemoved(data))
            .await
            .map_err(|e| {
                error!("Failed to queue custom domain removed job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_domain_removed".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_route_added(&self, data: String) -> Result<(), QueueServiceError> {
        info!("Queueing custom route added job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Route data cannot be empty".to_string(),
                job_type: "custom_route_added".to_string(),
            });
        }
        self.job_sender
            .send(Job::CustomRouteAdded(data))
            .await
            .map_err(|e| {
                error!("Failed to queue custom route added job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_route_added".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn launch_custom_route_removed(&self, data: String) -> Result<(), QueueServiceError> {
        info!("Queueing custom route removed job");
        if data.is_empty() {
            return Err(QueueServiceError::InvalidJobData {
                details: "Route data cannot be empty".to_string(),
                job_type: "custom_route_removed".to_string(),
            });
        }
        self.job_sender
            .send(Job::CustomRouteRemoved(data))
            .await
            .map_err(|e| {
                error!("Failed to queue custom route removed job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "custom_route_removed".to_string(),
                }
            })?;
        Ok(())
    }

    pub async fn queue_preset_calculation(
        &self,
        repository_id: i32,
    ) -> Result<(), QueueServiceError> {
        info!(
            "Queueing preset calculation job for repository: {}",
            repository_id
        );
        let job_data = CalculateRepositoryPresetJob { repository_id };
        self.job_sender
            .send(Job::CalculateRepositoryPreset(job_data))
            .await
            .map_err(|e| {
                error!("Failed to queue preset calculation job: {}", e);
                QueueServiceError::QueueSendError {
                    details: e.to_string(),
                    job_type: "calculate_repository_preset".to_string(),
                }
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::GitPushEventJob;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_publish_subscribe_git_push_job() {
        let (queue_service, mut receiver) = QueueService::create_channel(10);

        let job_data = GitPushEventJob {
            owner: "test-owner".to_string(),
            repo: "test-repo".to_string(),
            branch: Some("main".to_string()),
            tag: None,
            commit: "abc123def456".to_string(),
            project_id: 123,
        };

        // Publish job
        queue_service
            .job_sender
            .send(Job::GitPushEvent(job_data.clone()))
            .await
            .unwrap();

        // Subscribe/consume job
        let received_job = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("Should receive job within timeout")
            .expect("Should receive a job");

        match received_job {
            Job::GitPushEvent(received_data) => {
                assert_eq!(received_data.project_id, 123);
                assert_eq!(received_data.owner, "test-owner");
                assert_eq!(received_data.commit, "abc123def456");
            }
            _ => panic!("Expected GitPushEvent job"),
        }
    }

    #[tokio::test]
    async fn test_publish_subscribe_certificate_job() {
        let (queue_service, mut receiver) = QueueService::create_channel(10);

        let job_data = ProvisionCertificateJob {
            domain: "example.com".to_string(),
        };

        // Publish job
        queue_service
            .launch_certificate_provision(job_data.clone())
            .await
            .unwrap();

        // Subscribe/consume job
        let received_job = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("Should receive job within timeout")
            .expect("Should receive a job");

        match received_job {
            Job::ProvisionCertificate(received_data) => {
                assert_eq!(received_data.domain, "example.com");
            }
            _ => panic!("Expected ProvisionCertificate job"),
        }
    }

    #[tokio::test]
    async fn test_multiple_jobs_fifo_order() {
        let (queue_service, mut receiver) = QueueService::create_channel(10);

        // Publish multiple different jobs
        queue_service
            .launch_custom_domain_added("domain1.com".to_string())
            .await
            .unwrap();
        queue_service
            .launch_custom_domain_removed("domain2.com".to_string())
            .await
            .unwrap();
        queue_service
            .launch_custom_route_added("route1".to_string())
            .await
            .unwrap();

        // Consume jobs in FIFO order
        let job1 = receiver.recv().await.expect("Should receive first job");
        let job2 = receiver.recv().await.expect("Should receive second job");
        let job3 = receiver.recv().await.expect("Should receive third job");

        // Verify order and content
        match job1 {
            Job::CustomDomainAdded(domain) => assert_eq!(domain, "domain1.com"),
            _ => panic!("Expected CustomDomainAdded job first"),
        }

        match job2 {
            Job::CustomDomainRemoved(domain) => assert_eq!(domain, "domain2.com"),
            _ => panic!("Expected CustomDomainRemoved job second"),
        }

        match job3 {
            Job::CustomRouteAdded(route) => assert_eq!(route, "route1"),
            _ => panic!("Expected CustomRouteAdded job third"),
        }
    }

    #[tokio::test]
    async fn test_queue_service_clone() {
        let (queue_service, mut receiver) = QueueService::create_channel(10);

        // Clone the queue service
        let cloned_service = queue_service.clone();

        // Both services should be able to publish
        queue_service
            .launch_custom_domain_added("from_original".to_string())
            .await
            .unwrap();
        cloned_service
            .launch_custom_domain_added("from_clone".to_string())
            .await
            .unwrap();

        // Both jobs should be received
        let job1 = receiver.recv().await.expect("Should receive first job");
        let job2 = receiver.recv().await.expect("Should receive second job");

        let domains: Vec<String> = vec![job1, job2]
            .into_iter()
            .map(|job| match job {
                Job::CustomDomainAdded(domain) => domain,
                _ => panic!("Expected CustomDomainAdded job"),
            })
            .collect();

        assert!(domains.contains(&"from_original".to_string()));
        assert!(domains.contains(&"from_clone".to_string()));
    }

    #[tokio::test]
    async fn test_invalid_job_data_validation() {
        let (queue_service, _receiver) = QueueService::create_channel(10);

        // Test empty domain validation
        let result = queue_service
            .launch_custom_domain_added("".to_string())
            .await;
        assert!(result.is_err());

        match result.unwrap_err() {
            QueueServiceError::InvalidJobData { details, job_type } => {
                assert_eq!(details, "Domain cannot be empty");
                assert_eq!(job_type, "custom_domain_added");
            }
            _ => panic!("Expected InvalidJobData error"),
        }
    }

    #[tokio::test]
    async fn test_job_display_formatting() {
        let git_push_job = Job::GitPushEvent(GitPushEventJob {
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            branch: Some("main".to_string()),
            tag: None,
            commit: "abc123".to_string(),
            project_id: 123,
        });

        let cert_job = Job::ProvisionCertificate(ProvisionCertificateJob {
            domain: "test.com".to_string(),
        });

        assert!(format!("{}", git_push_job).contains("GitPushEvent"));
        assert_eq!(format!("{}", cert_job), "ProvisionCertificate(test.com)");
    }

    #[tokio::test]
    async fn test_broadcast_multiple_subscribers() {
        let (broadcast_service, _initial_receiver) =
            BroadcastQueueService::create_broadcast_channel(10);

        // Create multiple subscribers
        let mut subscriber1 = broadcast_service.subscribe();
        let mut subscriber2 = broadcast_service.subscribe();
        let mut subscriber3 = broadcast_service.subscribe();

        let job_data = ProvisionCertificateJob {
            domain: "multi-subscriber-test.com".to_string(),
        };

        // Broadcast job using available method
        broadcast_service
            .launch_certificate_provision(job_data.clone())
            .await
            .unwrap();

        // All subscribers should receive the same job
        let job1 = timeout(Duration::from_secs(1), subscriber1.recv())
            .await
            .expect("Subscriber 1 should receive job")
            .expect("Should receive a job");

        let job2 = timeout(Duration::from_secs(1), subscriber2.recv())
            .await
            .expect("Subscriber 2 should receive job")
            .expect("Should receive a job");

        let job3 = timeout(Duration::from_secs(1), subscriber3.recv())
            .await
            .expect("Subscriber 3 should receive job")
            .expect("Should receive a job");

        // Verify all received the same job
        for job in [job1, job2, job3] {
            match job {
                Job::ProvisionCertificate(received_data) => {
                    assert_eq!(received_data.domain, "multi-subscriber-test.com");
                }
                _ => panic!("Expected ProvisionCertificate job"),
            }
        }
    }

    #[tokio::test]
    async fn test_broadcast_late_subscriber() {
        let (broadcast_service, _initial_receiver) =
            BroadcastQueueService::create_broadcast_channel(10);

        // Send a job before subscriber exists (should be missed)
        broadcast_service
            .launch_custom_domain_added("missed.com".to_string())
            .await
            .unwrap();

        // Create subscriber after job was sent
        let mut late_subscriber = broadcast_service.subscribe();

        // Send another job after subscriber exists
        broadcast_service
            .launch_custom_domain_added("received.com".to_string())
            .await
            .unwrap();

        // Late subscriber should only receive the second job
        let received_job = timeout(Duration::from_secs(1), late_subscriber.recv())
            .await
            .expect("Should receive job within timeout")
            .expect("Should receive a job");

        match received_job {
            Job::CustomDomainAdded(domain) => {
                assert_eq!(domain, "received.com");
            }
            _ => panic!("Expected CustomDomainAdded job"),
        }

        // Verify no more jobs are available
        let result = timeout(Duration::from_millis(100), late_subscriber.recv()).await;
        assert!(result.is_err(), "Should not receive any more jobs");
    }
}
