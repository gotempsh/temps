// Re-export for convenience - consumer modules can just subscribe directly
pub use tokio::sync::broadcast;
pub use temps_core::Job;

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::{GitPushEventJob, ProvisionCertificateJob};
    use crate::BroadcastQueueService;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_simple_subscription_pattern() {
        use temps_core::JobQueue;

        let (queue, _) = BroadcastQueueService::create_broadcast_channel(10);
        let mut receiver = queue.subscribe(); // Direct subscription!

        // Send mixed jobs
        let git_push_job = GitPushEventJob {
            owner: "test-owner".to_string(),
            repo: "test-repo".to_string(),
            branch: Some("main".to_string()),
            tag: None,
            commit: "abc123".to_string(),
            project_id: 123,
        };
        queue.send(Job::GitPushEvent(git_push_job)).await.unwrap();

        queue.launch_certificate_provision(ProvisionCertificateJob {
            domain: "test.com".to_string(),
        }).await.unwrap();

        // Simple pattern matching - this is what consumer modules would do
        let job1 = receiver.recv().await.unwrap();
        match job1 {
            Job::GitPushEvent(data) => {
                assert_eq!(data.project_id, 123);
                assert_eq!(data.owner, "test-owner");
                // Handle git push job
            }
            _ => panic!("Expected git push job first"),
        }

        let job2 = receiver.recv().await.unwrap();
        match job2 {
            Job::ProvisionCertificate(data) => {
                assert_eq!(data.domain, "test.com");
                // Handle certificate job
            }
            _ => panic!("Expected certificate job second"),
        }
    }

    #[tokio::test]
    async fn test_filtering_pattern() {
        use temps_core::JobQueue;

        let (queue, _) = BroadcastQueueService::create_broadcast_channel(10);
        let mut receiver = queue.subscribe();

        // Send mixed jobs
        queue.launch_certificate_provision(ProvisionCertificateJob {
            domain: "example.com".to_string(),
        }).await.unwrap();

        let git_push_job = GitPushEventJob {
            owner: "acme-corp".to_string(),
            repo: "awesome-project".to_string(),
            branch: Some("develop".to_string()),
            tag: None,
            commit: "def456".to_string(),
            project_id: 999,
        };
        queue.send(Job::GitPushEvent(git_push_job)).await.unwrap();

        queue.launch_custom_domain_added("ignored.com".to_string()).await.unwrap();

        // Consumer module only caring about git push jobs would do this:
        let mut git_push_jobs = Vec::new();
        for _ in 0..3 {
            if let Ok(job) = timeout(Duration::from_millis(100), receiver.recv()).await {
                if let Ok(job) = job {
                    if let Job::GitPushEvent(data) = job {
                        git_push_jobs.push(data);
                    }
                    // Ignore other job types
                }
            }
        }

        assert_eq!(git_push_jobs.len(), 1);
        assert_eq!(git_push_jobs[0].project_id, 999);
    }

    #[tokio::test]
    async fn test_trait_based_usage() {
        use temps_core::{JobQueue, GitPushEventJob};

        // Consumer module would get a JobQueue trait object
        let queue: Box<dyn JobQueue> = crate::BroadcastQueueService::create_job_queue(10);
        let mut receiver = queue.subscribe();

        // Send job using the trait
        let git_push_job = GitPushEventJob {
            owner: "org".to_string(),
            repo: "project".to_string(),
            branch: Some("feature".to_string()),
            tag: None,
            commit: "xyz789".to_string(),
            project_id: 42,
        };
        queue.send(Job::GitPushEvent(git_push_job)).await.unwrap();

        // Receive using the trait
        let job = receiver.recv().await.unwrap();
        match job {
            Job::GitPushEvent(data) => {
                assert_eq!(data.project_id, 42);
                assert_eq!(data.owner, "org");
            }
            _ => panic!("Expected git push job"),
        }
    }
}