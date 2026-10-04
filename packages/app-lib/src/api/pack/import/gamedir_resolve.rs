use std::path::{Path, PathBuf};

use super::direct_link::has_minecraft_version_manifest;
use super::{ImportLauncherType, hmcl, pcl};

fn has_version_manifest(root: &Path) -> bool {
    std::fs::read_dir(root.join("versions")).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| has_minecraft_version_manifest(&entry.path()))
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedGamedir {
    pub path: String,
    pub dialect: String,
}

fn push_dir(result: &mut Vec<ResolvedGamedir>, path: PathBuf, dialect: &str) {
    if path.is_dir() {
        result.push(ResolvedGamedir {
            path: path.to_string_lossy().to_string(),
            dialect: dialect.to_string(),
        });
    }
}

pub fn resolve_gamedirs(
    launcher_type: ImportLauncherType,
    base_path: PathBuf,
) -> crate::Result<Vec<ResolvedGamedir>> {
    let mut result = Vec::new();
    match launcher_type {
        ImportLauncherType::Generic => {
            if !base_path.is_dir() || !has_version_manifest(&base_path) {
                return Err(crate::ErrorKind::InputError(
                    "Selected folder is not a Minecraft game directory"
                        .to_string(),
                )
                .into());
            }
            push_dir(&mut result, base_path, "generic")
        }
        ImportLauncherType::PCL2 | ImportLauncherType::PCL2CE => {
            for (_, path) in pcl::get_pcl_instances()
                .into_iter()
                .chain(pcl::get_pclce_instances())
            {
                push_dir(&mut result, PathBuf::from(path), "pcl");
            }
            if let Some((_, path)) = pcl::get_local_dotminecraft(&base_path) {
                push_dir(&mut result, PathBuf::from(path), "pcl");
            }
        }
        ImportLauncherType::HMCL => {
            for (_, path) in hmcl::get_instances(&base_path) {
                push_dir(&mut result, PathBuf::from(path), "hmcl");
            }
        }
        _ => {}
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    result.dedup_by(|a, b| Path::new(&a.path) == Path::new(&b.path));
    Ok(result)
}
