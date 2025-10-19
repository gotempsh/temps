use temps_core::notifications::{DynNotificationService, EmailMessage, NotificationError};

pub struct AuthEmailService {
    notification_service: DynNotificationService,
}

impl AuthEmailService {
    pub fn new(notification_service: DynNotificationService) -> Self {
        Self {
            notification_service,
        }
    }

    pub async fn send_verification_email(
        &self,
        email: &str,
        token: &str,
        base_url: &str,
    ) -> Result<(), NotificationError> {
        let message = EmailMessage {
            to: vec![email.to_string()],
            subject: "Verify your email".to_string(),
            body: format!("Your verification token is: {token}", token = token),
            html_body: Some(format!(
                r#"<!DOCTYPE html>
                <html>
                <head>
                    <style>
                        body {{ font-family: Arial, sans-serif; line-height: 1.6; }}
                        .container {{ max-width: 600px; margin: 0 auto; padding: 20px; }}
                        .button {{ background-color: #007bff; color: white; padding: 12px 24px; text-decoration: none; border-radius: 4px; display: inline-block; margin: 20px 0; }}
                    </style>
                </head>
                <body>
                    <div class="container">
                        <h2>Email Verification</h2>
                        <p>Please verify your email address by clicking the link below:</p>
                        <a href="{base_url}/auth/verify?token={token}" class="button">Verify Email</a>
                        <p>Or use this verification token: <strong>{token}</strong></p>
                        <p>If you didn't create an account, please ignore this email.</p>
                    </div>
                </body>
                </html>"#,
                base_url = base_url, token = token
            )),
            from: None,
            reply_to: None,
        };
        self.notification_service.send_email(message).await
    }

    pub async fn send_password_reset_email(
        &self,
        email: &str,
        token: &str,
        base_url: &str,
    ) -> Result<(), NotificationError> {
        let message = EmailMessage {
            to: vec![email.to_string()],
            subject: "Password Reset Request".to_string(),
            body: format!("Your password reset token is: {token}", token = token),
            html_body: Some(format!(
                r#"<!DOCTYPE html>
                <html>
                <head>
                    <style>
                        body {{ font-family: Arial, sans-serif; line-height: 1.6; }}
                        .container {{ max-width: 600px; margin: 0 auto; padding: 20px; }}
                        .button {{ background-color: #dc3545; color: white; padding: 12px 24px; text-decoration: none; border-radius: 4px; display: inline-block; margin: 20px 0; }}
                        .warning {{ color: #666; font-size: 14px; margin-top: 20px; }}
                    </style>
                </head>
                <body>
                    <div class="container">
                        <h2>Password Reset Request</h2>
                        <p>You requested a password reset. Click the link below to reset your password:</p>
                        <a href="{base_url}/auth/reset-password?token={token}" class="button">Reset Password</a>
                        <p>Or use this reset token: <strong>{token}</strong></p>
                        <p class="warning">This link will expire in 1 hour.</p>
                        <p class="warning">If you didn't request this, please ignore this email and your password will remain unchanged.</p>
                    </div>
                </body>
                </html>"#,
                base_url = base_url, token = token
            )),
            from: None,
            reply_to: None,
        };
        self.notification_service.send_email(message).await
    }

    pub async fn send_magic_link_email(
        &self,
        email: &str,
        magic_link_url: &str,
    ) -> Result<(), NotificationError> {
        let message = EmailMessage {
            to: vec![email.to_string()],
            subject: "Your Magic Login Link".to_string(),
            body: format!("Click here to login: {url}", url = magic_link_url),
            html_body: Some(format!(
                r#"<!DOCTYPE html>
                <html>
                <head>
                    <style>
                        body {{ font-family: Arial, sans-serif; line-height: 1.6; }}
                        .container {{ max-width: 600px; margin: 0 auto; padding: 20px; }}
                        .button {{ background-color: #28a745; color: white; padding: 12px 24px; text-decoration: none; border-radius: 4px; display: inline-block; margin: 20px 0; }}
                        .warning {{ color: #666; font-size: 14px; margin-top: 20px; }}
                    </style>
                </head>
                <body>
                    <div class="container">
                        <h2>Magic Link Login</h2>
                        <p>Click the link below to instantly log in to your account:</p>
                        <a href="{url}" class="button">Login Now</a>
                        <p class="warning">This link will expire in 15 minutes.</p>
                        <p class="warning">If you didn't request this, please ignore this email.</p>
                    </div>
                </body>
                </html>"#,
                url = magic_link_url
            )),
            from: None,
            reply_to: None,
        };
        self.notification_service.send_email(message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use async_trait::async_trait;
    use temps_core::notifications::EmailMessage;

    #[derive(Clone)]
    struct MockNotificationService {
        sent_emails: Arc<tokio::sync::Mutex<Vec<EmailMessage>>>,
    }

    impl MockNotificationService {
        fn new() -> Self {
            Self {
                sent_emails: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            }
        }

        async fn get_sent_emails(&self) -> Vec<EmailMessage> {
            self.sent_emails.lock().await.clone()
        }
    }

    #[async_trait]
    impl temps_core::notifications::NotificationService for MockNotificationService {
        async fn send_email(&self, message: EmailMessage) -> Result<(), NotificationError> {
            self.sent_emails.lock().await.push(message);
            Ok(())
        }

        async fn send_notification(&self, notification: temps_core::notifications::NotificationData) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn test_send_verification_email() {
        let mock_service = Arc::new(MockNotificationService::new());
        let email_service = AuthEmailService::new(mock_service.clone());

        let result = email_service.send_verification_email(
            "test@example.com",
            "test-token-123",
            "https://example.com"
        ).await;

        assert!(result.is_ok());

        let sent_emails = mock_service.get_sent_emails().await;
        assert_eq!(sent_emails.len(), 1);

        let email = &sent_emails[0];
        assert_eq!(email.to, vec!["test@example.com"]);
        assert_eq!(email.subject, "Verify your email");
        assert!(email.body.contains("test-token-123"));

        let html = email.html_body.as_ref().unwrap();
        assert!(html.contains("https://example.com/auth/verify?token=test-token-123"));
        assert!(html.contains("test-token-123"));
    }

    #[tokio::test]
    async fn test_send_password_reset_email() {
        let mock_service = Arc::new(MockNotificationService::new());
        let email_service = AuthEmailService::new(mock_service.clone());

        let result = email_service.send_password_reset_email(
            "user@example.com",
            "reset-token-456",
            "https://app.example.com"
        ).await;

        assert!(result.is_ok());

        let sent_emails = mock_service.get_sent_emails().await;
        assert_eq!(sent_emails.len(), 1);

        let email = &sent_emails[0];
        assert_eq!(email.to, vec!["user@example.com"]);
        assert_eq!(email.subject, "Password Reset Request");
        assert!(email.body.contains("reset-token-456"));

        let html = email.html_body.as_ref().unwrap();
        assert!(html.contains("https://app.example.com/auth/reset-password?token=reset-token-456"));
        assert!(html.contains("reset-token-456"));
        assert!(html.contains("This link will expire in 1 hour"));
    }

    #[tokio::test]
    async fn test_send_magic_link_email() {
        let mock_service = Arc::new(MockNotificationService::new());
        let email_service = AuthEmailService::new(mock_service.clone());

        let magic_url = "https://example.com/auth/magic?token=magic-789";
        let result = email_service.send_magic_link_email(
            "magic@example.com",
            magic_url
        ).await;

        assert!(result.is_ok());

        let sent_emails = mock_service.get_sent_emails().await;
        assert_eq!(sent_emails.len(), 1);

        let email = &sent_emails[0];
        assert_eq!(email.to, vec!["magic@example.com"]);
        assert_eq!(email.subject, "Your Magic Login Link");
        assert!(email.body.contains(magic_url));

        let html = email.html_body.as_ref().unwrap();
        assert!(html.contains(magic_url));
        assert!(html.contains("This link will expire in 15 minutes"));
    }

    #[tokio::test]
    async fn test_email_html_formatting() {
        let mock_service = Arc::new(MockNotificationService::new());
        let email_service = AuthEmailService::new(mock_service.clone());

        let _ = email_service.send_verification_email(
            "format@example.com",
            "token",
            "https://test.com"
        ).await;

        let sent_emails = mock_service.get_sent_emails().await;
        let html = sent_emails[0].html_body.as_ref().unwrap();

        // Check HTML structure
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<html>"));
        assert!(html.contains("<head>"));
        assert!(html.contains("<style>"));
        assert!(html.contains("<body>"));
        assert!(html.contains("class=\"container\""));
        assert!(html.contains("class=\"button\""));
    }

    #[tokio::test]
    async fn test_named_parameters_in_templates() {
        let mock_service = Arc::new(MockNotificationService::new());
        let email_service = AuthEmailService::new(mock_service.clone());

        // Test verification email
        let _ = email_service.send_verification_email(
            "param@example.com",
            "unique-token",
            "https://unique-base.com"
        ).await;

        let sent_emails = mock_service.get_sent_emails().await;
        let html = sent_emails[0].html_body.as_ref().unwrap();

        // Verify named parameters are properly substituted
        assert!(html.contains("https://unique-base.com/auth/verify?token=unique-token"));
        assert!(!html.contains("{base_url}"));
        assert!(!html.contains("{token}"));
    }
}
