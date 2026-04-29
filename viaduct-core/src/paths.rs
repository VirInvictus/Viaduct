// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use std::env;
use std::path::PathBuf;

use crate::error::{Result, ViaductError};

const APP_DIR: &str = "viaduct";

pub fn data_dir() -> Result<PathBuf> {
    Ok(xdg_home("XDG_DATA_HOME", ".local/share")?.join(APP_DIR))
}

pub fn cache_dir() -> Result<PathBuf> {
    Ok(xdg_home("XDG_CACHE_HOME", ".cache")?.join(APP_DIR))
}

pub fn opml_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("local.opml"))
}

pub fn articles_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("articles.sqlite"))
}

pub fn feed_settings_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("feed-settings.sqlite"))
}

pub fn sync_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("sync.sqlite"))
}

pub fn favicon_cache_dir() -> Result<PathBuf> {
    Ok(cache_dir()?.join("favicons"))
}

pub fn image_cache_dir() -> Result<PathBuf> {
    Ok(cache_dir()?.join("images"))
}

pub fn video_thumb_cache_dir() -> Result<PathBuf> {
    Ok(cache_dir()?.join("video-thumbs"))
}

pub fn fonts_dir() -> Result<PathBuf> {
    Ok(xdg_home("XDG_DATA_HOME", ".local/share")?
        .join("fonts")
        .join(APP_DIR))
}

pub fn ensure_dirs() -> Result<()> {
    for dir in [
        data_dir()?,
        cache_dir()?,
        favicon_cache_dir()?,
        image_cache_dir()?,
        video_thumb_cache_dir()?,
        fonts_dir()?,
    ] {
        std::fs::create_dir_all(&dir).map_err(|source| ViaductError::CreateDir {
            path: dir.clone(),
            source,
        })?;
    }
    Ok(())
}

fn xdg_home(var: &str, fallback_under_home: &str) -> Result<PathBuf> {
    if let Ok(value) = env::var(var)
        && !value.is_empty()
    {
        return Ok(PathBuf::from(value));
    }
    let home = env::var("HOME").map_err(|_| ViaductError::MissingHome)?;
    Ok(PathBuf::from(home).join(fallback_under_home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = vars.iter().map(|(k, _)| (*k, env::var(k).ok())).collect();
        for (k, v) in vars {
            match v {
                Some(val) => unsafe { env::set_var(k, val) },
                None => unsafe { env::remove_var(k) },
            }
        }
        f();
        for (k, original) in saved {
            match original {
                Some(val) => unsafe { env::set_var(k, val) },
                None => unsafe { env::remove_var(k) },
            }
        }
    }

    #[test]
    fn data_dir_honors_xdg_data_home() {
        with_env(&[("XDG_DATA_HOME", Some("/tmp/xdg-data"))], || {
            assert_eq!(data_dir().unwrap(), PathBuf::from("/tmp/xdg-data/viaduct"));
        });
    }

    #[test]
    fn cache_dir_honors_xdg_cache_home() {
        with_env(&[("XDG_CACHE_HOME", Some("/tmp/xdg-cache"))], || {
            assert_eq!(
                cache_dir().unwrap(),
                PathBuf::from("/tmp/xdg-cache/viaduct")
            );
        });
    }

    #[test]
    fn data_dir_falls_back_to_home() {
        with_env(
            &[("XDG_DATA_HOME", None), ("HOME", Some("/home/testuser"))],
            || {
                assert_eq!(
                    data_dir().unwrap(),
                    PathBuf::from("/home/testuser/.local/share/viaduct")
                );
            },
        );
    }

    #[test]
    fn empty_xdg_var_falls_back_to_home() {
        with_env(
            &[
                ("XDG_DATA_HOME", Some("")),
                ("HOME", Some("/home/testuser")),
            ],
            || {
                assert_eq!(
                    data_dir().unwrap(),
                    PathBuf::from("/home/testuser/.local/share/viaduct")
                );
            },
        );
    }
}
