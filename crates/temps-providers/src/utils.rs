use bollard::{models::NetworkCreateRequest, query_parameters::ListNetworksOptions, Docker};
use tracing::{error, info};

pub(crate) async fn ensure_network_exists(
    docker: &Docker,
) -> Result<(), Box<dyn std::error::Error>> {
    const NETWORK_NAME: &str = "temps-app-network";

    // Check if network exists
    let networks = docker.list_networks(None::<ListNetworksOptions>).await?;
    let network_exists = networks
        .iter()
        .any(|n| n.name.as_deref() == Some(NETWORK_NAME));

    if !network_exists {
        info!("Creating network: {}", NETWORK_NAME);
        let options = NetworkCreateRequest {
            name: NETWORK_NAME.to_string(),
            driver: Some("bridge".to_string()),
            ..Default::default()
        };

        match docker.create_network(options).await {
            Ok(_) => info!("Successfully created network: {}", NETWORK_NAME),
            Err(e) => {
                error!("Failed to create network: {}", e);
                return Err(Box::new(e));
            }
        }
    }

    Ok(())
}
