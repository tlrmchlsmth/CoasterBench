//! The site's typefaces, compiled in and written beside the pages. Silkscreen
//! is the chrome, IBM Plex Mono the content; see CLAUDE.local.md.

use std::path::Path;

use anyhow::{Context, Result};

/// Written to `assets/fonts/`.
const FACES: [(&str, &[u8]); 4] = [
    (
        "Silkscreen-Regular.woff2",
        include_bytes!("../fonts/Silkscreen-Regular.woff2"),
    ),
    (
        "Silkscreen-Bold.woff2",
        include_bytes!("../fonts/Silkscreen-Bold.woff2"),
    ),
    (
        "IBMPlexMono-Regular.woff2",
        include_bytes!("../fonts/IBMPlexMono-Regular.woff2"),
    ),
    (
        "IBMPlexMono-Bold.woff2",
        include_bytes!("../fonts/IBMPlexMono-Bold.woff2"),
    ),
];

/// Preloaded: the first screen is body text.
pub const PRELOAD: &str = "assets/fonts/IBMPlexMono-Regular.woff2";

pub fn write(out: &Path) -> Result<()> {
    let dir = out.join("assets").join("fonts");
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in FACES {
        let path = dir.join(name);
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}
