// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::Result;
use ashpd::desktop::background::Background;

pub async fn request_background_permission() -> Result<bool> {
    let response = Background::request()
        .reason("Viaduct needs to refresh feeds in the background to keep your news up to date.")
        .auto_start(true)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Background portal request failed: {}", e))?
        .response()
        .map_err(|e| anyhow::anyhow!("Background portal response failed: {}", e))?;

    Ok(response.run_in_background())
}
