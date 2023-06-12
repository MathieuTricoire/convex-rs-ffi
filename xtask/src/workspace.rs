use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use xshell::{cmd, Shell};

use crate::utils::cargo_path;

#[derive(Deserialize)]
pub struct Metadata {
    #[serde(rename = "workspace_root")]
    pub root_dir: PathBuf,
    #[serde(rename = "target_directory")]
    pub target_dir: PathBuf,
}

pub fn metadata() -> Result<Metadata> {
    let sh = Shell::new()?;
    let cargo = cargo_path();
    let metadata_json = cmd!(sh, "{cargo} metadata --no-deps --format-version 1").read()?;
    Ok(serde_json::from_str::<Metadata>(&metadata_json)?)
}
