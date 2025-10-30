use bollard::{models::NetworkCreateRequest, query_parameters::ListNetworksOptions, Docker};
use tracing::{error, info};

pub(crate) async fn ensure_network_exists(
    docker: &Docker,
) -> Result<(), Box<dyn std::error::Error>> {
    let network_name = temps_core::NETWORK_NAME.as_str();

    // Check if network exists
    let networks = docker.list_networks(None::<ListNetworksOptions>).await?;
    let network_exists = networks
        .iter()
        .any(|n| n.name.as_deref() == Some(network_name));

    if !network_exists {
        info!("Creating network: {}", network_name);
        let options = NetworkCreateRequest {
            name: network_name.to_string(),
            driver: Some("bridge".to_string()),
            ..Default::default()
        };

        match docker.create_network(options).await {
            Ok(_) => info!("Successfully created network: {}", network_name),
            Err(e) => {
                error!("Failed to create network: {}", e);
                return Err(Box::new(e));
            }
        }
    }

    Ok(())
}
