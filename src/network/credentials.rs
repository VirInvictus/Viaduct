// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::Result;
use oo7::dbus::Service;

pub struct Credentials {
    pub username: String,
    pub password: Option<String>,
}

pub async fn store_credentials(account_id: &str, creds: &Credentials) -> Result<()> {
    let service = Service::new().await.map_err(|e| anyhow::anyhow!("Failed to connect to Secret Service: {}", e))?;
    let collection = service.default_collection().await.map_err(|e| anyhow::anyhow!("Failed to open default collection: {}", e))?;
    
    let mut attributes = std::collections::HashMap::new();
    attributes.insert("account-id", account_id);
    attributes.insert("application", "viaduct");

    if let Some(password) = &creds.password {
        collection.create_item(
            &format!("viaduct: {}", creds.username),
            &attributes,
            password.as_bytes(),
            true,
            None,
        ).await.map_err(|e| anyhow::anyhow!("Failed to store secret: {}", e))?;
    }
    
    Ok(())
}

pub async fn fetch_credentials(account_id: &str) -> Result<Option<Credentials>> {
    let service = Service::new().await.map_err(|e| anyhow::anyhow!("Failed to connect to Secret Service: {}", e))?;
    let collection = service.default_collection().await.map_err(|e| anyhow::anyhow!("Failed to open default collection: {}", e))?;
    
    let mut attributes = std::collections::HashMap::new();
    attributes.insert("account-id", account_id);
    attributes.insert("application", "viaduct");

    let items = collection.search_items(&attributes).await.map_err(|e| anyhow::anyhow!("Failed to search secrets: {}", e))?;
    if let Some(item) = items.first() {
        let secret = item.secret().await.map_err(|e| anyhow::anyhow!("Failed to fetch secret: {}", e))?;
        let password = String::from_utf8(secret.to_vec()).ok();
        let label = item.label().await.map_err(|e| anyhow::anyhow!("Failed to fetch label: {}", e))?;
        let username = label.strip_prefix("viaduct: ").unwrap_or(&label).to_string();
        
        return Ok(Some(Credentials {
            username,
            password,
        }));
    }

    Ok(None)
}
