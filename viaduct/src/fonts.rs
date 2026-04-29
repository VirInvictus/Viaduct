// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Bundled-font installation. Lives in the binary crate (rather than
//! `viaduct-core::paths`) because installing fonts and refreshing
//! `fc-cache` is a GTK-runtime concern — the fonts only matter for the
//! WebKit article pane's typography, not for any headless code path.
//!
//! Run from `main.rs` after `paths::ensure_dirs` so the `~/.local/share/fonts/viaduct`
//! target directory exists before we drop bytes into it. Idempotent: each
//! font is written only when the file isn't already present.

use viaduct_core::error::Result;
use viaduct_core::paths::fonts_dir;

pub fn install_bundled() -> Result<()> {
    let target_dir = fonts_dir()?;
    let fonts = [
        (
            "Inter-Regular.ttf",
            include_bytes!("../../data/fonts/Inter-Regular.ttf").as_slice(),
        ),
        (
            "Inter-Bold.ttf",
            include_bytes!("../../data/fonts/Inter-Bold.ttf").as_slice(),
        ),
        (
            "SourceSerif4-Regular.ttf",
            include_bytes!("../../data/fonts/SourceSerif4-Regular.ttf").as_slice(),
        ),
        (
            "SourceSerif4-Bold.ttf",
            include_bytes!("../../data/fonts/SourceSerif4-Bold.ttf").as_slice(),
        ),
        (
            "JetBrainsMono-Regular.ttf",
            include_bytes!("../../data/fonts/JetBrainsMono-Regular.ttf").as_slice(),
        ),
    ];

    let mut changed = false;
    for (name, bytes) in fonts {
        let path = target_dir.join(name);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!("Failed to install bundled font {}: {}", name, e);
            } else {
                changed = true;
            }
        }
    }

    if changed {
        tracing::info!(
            "Installed bundled fonts to {}. Rebuilding font cache...",
            target_dir.display()
        );
        let _ = std::process::Command::new("fc-cache")
            .arg("-f")
            .arg(&target_dir)
            .status();
    }
    Ok(())
}
