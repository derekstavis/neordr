//! Shared focus for Moshi's session-less terminal attachment. Like a Herdr UI,
//! default attachments share focus; explicitly named attachments remain pinned.
use crate::session;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Deserialize, Serialize)]
struct Selection {
    name: String,
    generation: Option<String>,
}

pub fn remember(config: &Path, name: &str) -> Result<()> {
    let instance = session::inspect_bridge(&session::socket_path(config, name)?)?;
    let root = config.join("herdr");
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = root.join(format!(
        ".neordr-focus-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        serde_json::to_writer(
            &mut file,
            &Selection {
                name: name.into(),
                generation: instance.generation,
            },
        )?;
        file.flush()?;
        fs::rename(&temporary, root.join(".neordr-focus.json"))?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

pub fn current(config: &Path) -> Result<Option<String>> {
    let path = config.join("herdr/.neordr-focus.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let selected: Selection = serde_json::from_slice(&bytes)?;
    let socket = session::socket_path(config, &selected.name)?;
    match session::inspect_bridge(&socket) {
        Ok(instance) if instance.generation == selected.generation => Ok(Some(selected.name)),
        _ => Ok(None), // Never reuse a selection from a previous editor instance.
    }
}
