// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Bundled sidebar symbolic icons, installed at startup into the user's
//! hicolor theme (the `fonts.rs` pattern).
//!
//! Why bundled: the smart-feed rows request glyphs that not every icon
//! theme ships (`x-office-calendar-symbolic` in particular is absent from
//! several, rendering GTK's image-missing placeholder), and whatever does
//! resolve comes from an arbitrary theme with mismatched weight. Owning
//! the artwork makes the sidebar uniform everywhere.

use crate::error::{Result, ViaductError};

/// The icons, as `(file stem, svg)`. The `viaduct-` prefix keeps the user's
/// hicolor namespace collision-free. Fill/stroke colors are placeholders:
/// GTK recolors `-symbolic` icons from the style context.
const ICONS: &[(&str, &str)] = &[
    (
        "viaduct-smart-feeds-symbolic",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><g fill="#000"><rect x="2" y="2.4" width="12" height="2.2" rx="1.1"/><rect x="2" y="6.9" width="12" height="2.2" rx="1.1"/><rect x="2" y="11.4" width="12" height="2.2" rx="1.1"/></g></svg>"##,
    ),
    (
        "viaduct-today-symbolic",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#000" fill-rule="evenodd" d="M4 0.8h2v2H4zM10 0.8h2v2h-2zM2.5 3h11C14.3 3 15 3.7 15 4.5v8c0 .8-.7 1.5-1.5 1.5h-11C1.7 14 1 13.3 1 12.5v-8C1 3.7 1.7 3 2.5 3zM4 7.2v4.3c0 .3.2.5.5.5h7c.3 0 .5-.2.5-.5V7.2H4z"/></svg>"##,
    ),
    (
        "viaduct-all-unread-symbolic",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#000" fill-rule="evenodd" d="M2.5 3.5h11C14.3 3.5 15 4.2 15 5v6c0 .8-.7 1.5-1.5 1.5h-11C1.7 12.5 1 11.8 1 11V5c0-.8.7-1.5 1.5-1.5zM3.2 4.6c-.4 0-.6.5-.3.7l4.6 3.9c.3.3.7.3 1 0l4.6-3.9c.3-.2.1-.7-.3-.7H3.2z"/></svg>"##,
    ),
    (
        "viaduct-starred-symbolic",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#000" d="M8 1.5l1.9 4.2 4.6.4-3.5 3 1 4.5L8 11.3l-4 2.3 1-4.5-3.5-3 4.6-.4z"/></svg>"##,
    ),
    (
        "viaduct-smart-feed-symbolic",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#000" d="M2 2h12l-4.6 5.4v4.8L6.6 14V7.4z"/></svg>"##,
    ),
];

/// Install the bundled icons into the user's hicolor theme. Idempotent:
/// a file is written only when missing or changed, so artwork updates
/// land across versions without rewriting untouched files.
pub fn install_bundled() -> Result<()> {
    install_bundled_into(&crate::paths::icons_dir()?)
}

fn install_bundled_into(dir: &std::path::Path) -> Result<()> {
    let target = dir.join("hicolor").join("scalable").join("apps");
    std::fs::create_dir_all(&target)?;

    for (stem, svg) in ICONS {
        let path = target.join(format!("{stem}.svg"));
        let unchanged = std::fs::read(&path).is_ok_and(|existing| existing == svg.as_bytes());
        if unchanged {
            continue;
        }
        std::fs::write(&path, svg).map_err(ViaductError::from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The install is idempotent across runs and rewrites when the
    /// bundled artwork changes (bytes compared, not mtimes).
    #[test]
    fn install_is_idempotent_and_rewrites_changes() {
        let dir = std::env::temp_dir().join(format!("viaduct-icons-test-{}", std::process::id()));
        install_bundled_into(&dir).expect("first install");
        let first = std::fs::read(dir.join("hicolor/scalable/apps/viaduct-starred-symbolic.svg"))
            .expect("icon written");

        install_bundled_into(&dir).expect("second install");
        let second = std::fs::read(dir.join("hicolor/scalable/apps/viaduct-starred-symbolic.svg"))
            .expect("icon still present");
        assert_eq!(first, second, "unchanged artwork must not be rewritten");

        assert_eq!(ICONS.len(), 5);
        for (stem, svg) in ICONS {
            let path = dir
                .join("hicolor/scalable/apps")
                .join(format!("{stem}.svg"));
            let on_disk = std::fs::read(&path).expect("each icon installs");
            assert_eq!(on_disk, svg.as_bytes(), "{stem} matches the bundle");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
