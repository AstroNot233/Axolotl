//! Publication of portable versions after internal loader installation completes.

use std::path::Path;

use daedalus::minecraft::{DownloadType, VersionInfo};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::direct_ensure::{self, LinkedFilePlan};
use super::direct_link::{
    DirectLinkedLaunch, classified_artifact_path, is_native_only_library,
};
use super::download::MinecraftDownloadProgress;
use super::instance_runtime::InstanceRuntimeAdapter;
use super::local_version::LinkedLibrary;
use crate::instance::QuickPlayType;
use crate::state::{Instance, State};
use crate::util::{fetch, io};

pub(crate) struct PortableVersion {
    pub(crate) document: Value,
    pub(crate) libraries: Vec<LinkedLibrary>,
}

pub(crate) fn project_manifest(
    info: &VersionInfo,
    version_name: &str,
    game_version: &str,
) -> crate::Result<PortableVersion> {
    let libraries = info
        .libraries
        .iter()
        .filter(|library| {
            !library
                .name
                .starts_with("com.axolotl.loader-installer:embedded:")
                && (library.include_in_classpath
                    || library.natives.is_some()
                    || is_native_only_library(library))
        })
        .map(|library| LinkedLibrary {
            library: library.clone(),
            hint: None,
            filename: None,
        })
        .collect::<Vec<_>>();
    let mut document = serde_json::to_value(info)?;
    document["id"] = version_name.into();
    document["clientVersion"] = game_version.into();
    let object = document
        .as_object_mut()
        .expect("VersionInfo serializes as an object");
    object.remove("data");
    object.remove("processors");
    let portable_libraries = libraries
        .iter()
        .map(|library| {
            let mut value = serde_json::to_value(&library.library)?;
            let object = value
                .as_object_mut()
                .expect("Library serializes as an object");
            object.remove("include_in_classpath");
            object.remove("downloadable");
            if !library.library.include_in_classpath
                && let Some(downloads) =
                    object.get_mut("downloads").and_then(Value::as_object_mut)
            {
                downloads.remove("artifact");
            }
            Ok(value)
        })
        .collect::<crate::Result<Vec<_>>>()?;
    document["libraries"] = portable_libraries.into();
    Ok(PortableVersion {
        document,
        libraries,
    })
}

fn check_cancellation(cancellation: &CancellationToken) -> crate::Result<()> {
    if cancellation.is_cancelled() {
        return Err(crate::ErrorKind::LauncherError(
            "External version installation canceled".to_string(),
        )
        .into());
    }
    Ok(())
}

fn applicable(
    library: &LinkedLibrary,
    java_arch: &str,
    minecraft_updated: bool,
) -> bool {
    library.library.rules.as_deref().is_none_or(|rules| {
        super::parse_rules(
            rules,
            java_arch,
            &QuickPlayType::None,
            minecraft_updated,
        )
    })
}

async fn copy_runtime_artifact(
    state: &State,
    source: &Path,
    plan: &LinkedFilePlan,
    required: bool,
) -> crate::Result<()> {
    if !source.is_file() {
        if required {
            return Err(crate::ErrorKind::LauncherError(format!(
                "Missing generated runtime artifact {} at {} (external destination {})",
                plan.label, source.display(), plan.destination.display()
            )).into());
        }
        return Ok(());
    }
    let sha1 = match &plan.sha1 {
        Some(sha1) => sha1.clone(),
        None => fetch::sha1_file_async(source).await?.1,
    };
    if direct_ensure::file_is_current(&plan.destination, Some(&sha1), plan.size)
        .await
    {
        return Ok(());
    }
    let copied = super::local_artifact::copy_verified(
        source,
        &plan.destination,
        Some(&sha1),
        plan.size,
        &state.io_semaphore,
    )
    .await
    .map_err(|error| {
        crate::ErrorKind::LauncherError(format!(
            "Could not copy runtime artifact {} from {} to {}: {error}",
            plan.label,
            source.display(),
            plan.destination.display()
        ))
    })?;
    if !copied && required {
        return Err(crate::ErrorKind::LauncherError(format!(
            "Invalid generated runtime artifact {} at {} (external destination {})",
            plan.label, source.display(), plan.destination.display()
        )).into());
    }
    Ok(())
}

async fn copy_runtime_libraries(
    state: &State,
    direct: &DirectLinkedLaunch,
    libraries: &[LinkedLibrary],
    java_arch: &str,
    minecraft_updated: bool,
    cancellation: &CancellationToken,
) -> crate::Result<()> {
    let cache = state.directories.libraries_dir();
    for library in libraries {
        check_cancellation(cancellation)?;
        if !applicable(library, java_arch, minecraft_updated) {
            continue;
        }
        let lib = &library.library;
        let mut plans = Vec::new();
        if lib.include_in_classpath
            && let Some(plan) =
                direct_ensure::linked_classpath_plan(direct, library)?
        {
            let relative = library.classpath_relative_path()?;
            let declared_source = cache.join(&relative);
            let source = if declared_source.is_file() {
                declared_source
            } else {
                cache.join(daedalus::get_path_from_artifact(&lib.name)?)
            };
            plans.push((source, plan, !lib.downloadable));
        }
        if let Some(plan) =
            direct_ensure::linked_native_plan(direct, library, java_arch)?
        {
            let relative = plan
                .destination
                .strip_prefix(direct.libraries_dir())
                .map_err(|error| {
                    crate::ErrorKind::LauncherError(format!(
                        "Invalid external native path: {error}"
                    ))
                })?
                .to_path_buf();
            plans.push((cache.join(relative), plan, !lib.downloadable));
        }
        if let Some(classifiers) = lib
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.classifiers.as_ref())
        {
            for (classifier, download) in classifiers {
                if !classifier.starts_with("native")
                    && !lib.natives.as_ref().is_some_and(|natives| {
                        natives.values().any(|pattern| {
                            ["32", "64"].iter().any(|width| {
                                pattern.replace("${arch}", width) == *classifier
                            })
                        })
                    })
                {
                    continue;
                }
                let relative = download.path.clone().map_or_else(
                    || classified_artifact_path(&lib.name, classifier),
                    Ok,
                )?;
                if !direct_ensure::safe_maven_relative_path(&relative) {
                    return Err(crate::ErrorKind::LauncherError(format!(
                        "Refusing unsafe classifier path {relative:?} for {}",
                        lib.name
                    ))
                    .into());
                }
                let destination = direct.libraries_dir().join(&relative);
                if plans
                    .iter()
                    .any(|(_, plan, _)| plan.destination == destination)
                {
                    continue;
                }
                plans.push((
                    cache.join(&relative),
                    LinkedFilePlan {
                        label: format!("{}:{classifier}", lib.name),
                        urls: Vec::new(),
                        destination,
                        sha1: (!download.sha1.trim().is_empty())
                            .then(|| download.sha1.clone()),
                        size: (download.size > 0)
                            .then_some(download.size as u64),
                        validation: fetch::ContentValidation::Jar,
                    },
                    false,
                ));
            }
        }
        for (source, plan, required) in plans {
            check_cancellation(cancellation)?;
            copy_runtime_artifact(state, &source, &plan, required).await?;
        }
    }
    Ok(())
}

pub(crate) async fn finalize_external_version(
    instance: &Instance,
    version_id: &str,
    game_version: &str,
    info: &VersionInfo,
    state: &State,
    java_arch: &str,
    minecraft_updated: bool,
    progress: Option<&MinecraftDownloadProgress>,
    cancellation: &CancellationToken,
) -> crate::Result<()> {
    let runtime =
        InstanceRuntimeAdapter::for_instance(instance, &state.directories)?;
    let Some(direct) = runtime.direct_link() else {
        return Ok(());
    };
    finalize_version(
        direct,
        version_id,
        game_version,
        info,
        state,
        java_arch,
        minecraft_updated,
        progress,
        cancellation,
    )
    .await
}

async fn finalize_version(
    direct: &DirectLinkedLaunch,
    version_id: &str,
    game_version: &str,
    info: &VersionInfo,
    state: &State,
    java_arch: &str,
    minecraft_updated: bool,
    progress: Option<&MinecraftDownloadProgress>,
    cancellation: &CancellationToken,
) -> crate::Result<()> {
    check_cancellation(cancellation)?;
    let portable = project_manifest(info, &direct.version_id, game_version)?;
    copy_runtime_libraries(
        state,
        direct,
        &portable.libraries,
        java_arch,
        minecraft_updated,
        cancellation,
    )
    .await?;
    let client = info.downloads.get(&DownloadType::Client);
    let source = state
        .directories
        .version_dir(version_id)
        .join(format!("{version_id}.jar"));
    let plan = LinkedFilePlan {
        label: format!("Minecraft client {game_version}"),
        urls: Vec::new(),
        destination: direct
            .version_dir()
            .join(format!("{}.jar", direct.version_id)),
        sha1: client
            .filter(|client| !client.sha1.is_empty())
            .map(|client| client.sha1.clone()),
        size: client
            .filter(|client| client.size > 0)
            .map(|client| client.size as u64),
        validation: fetch::ContentValidation::Jar,
    };
    check_cancellation(cancellation)?;
    copy_runtime_artifact(state, &source, &plan, true).await?;
    direct_ensure::ensure_direct_launch_dependencies_with_progress(
        state,
        direct,
        &portable.libraries,
        info,
        java_arch,
        minecraft_updated,
        progress,
    )
    .await?;
    for library in &portable.libraries {
        if applicable(library, java_arch, minecraft_updated) {
            direct_ensure::validate_local_runtime_library(
                direct, library, java_arch,
            )
            .await?;
        }
    }
    check_cancellation(cancellation)?;
    let json = direct
        .version_dir()
        .join(format!("{}.json", direct.version_id));
    let lock = fetch::destination_download_lock(&json);
    let _guard = lock.lock().await;
    check_cancellation(cancellation)?;
    io::write(json, serde_json::to_vec(&portable.document)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::{ExternalGameDirMode, LinkedLauncherDialect};
    use crate::state::{DirectoryInfo, test_state};
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn version_info() -> VersionInfo {
        serde_json::from_value(json!({
            "id": "1.20.1-forge", "assets": "1.20",
            "assetIndex": {"id": "", "sha1": "", "size": 0, "totalSize": 0, "url": ""},
            "downloads": {}, "libraries": [],
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "arguments": {"game": ["--fml.mcVersion", "1.20.1"]},
            "minimumLauncherVersion": 21,
            "releaseTime": "2023-06-12T13:25:51Z", "time": "2023-06-13T11:08:00Z",
            "type": "release",
            "data": {"BINPATCH": {"client": "private-cache-path", "server": ""}},
            "processors": [{"jar": "processor:tool:1", "classpath": [], "args": []}]
        })).unwrap()
    }

    fn runtime_libraries() -> Vec<daedalus::minecraft::Library> {
        serde_json::from_value(json!([
            {"name": "net.minecraftforge:forge:1.20.1-47.4.0:client", "downloadable": false,
                "downloads": {"artifact": {"path": "custom/forge-client.jar", "sha1": "", "size": 0, "url": ""}}},
            {"name": "optifine:OptiFine:1.20.1_HD_U_I6", "downloadable": false},
            {"name": "example:runtime:1.0"},
            {"name": "processor:tool:1", "include_in_classpath": false},
            {"name": "com.axolotl.loader-installer:embedded:1:client@lzma", "include_in_classpath": false, "downloadable": false}
        ])).unwrap()
    }

    async fn fixture() -> (TempDir, Arc<State>, DirectLinkedLaunch) {
        let temp = TempDir::new().unwrap();
        let dirs = DirectoryInfo {
            settings_dir: temp.path().join("settings"),
            config_dir: temp.path().join("config"),
            app_identifier: "test".to_string(),
        };
        std::fs::create_dir_all(dirs.instances_dir()).unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let state = test_state(dirs, pool).await.unwrap();
        let direct = DirectLinkedLaunch {
            dot_minecraft: temp.path().join(".minecraft"),
            launcher_root: None,
            version_id: "Survival".to_string(),
            version_json: None,
            dialect: LinkedLauncherDialect::PclCe,
            game_dir_mode: Some(ExternalGameDirMode::Isolated),
        };
        (temp, state, direct)
    }

    fn write(path: impl AsRef<Path>, bytes: &[u8]) {
        let path = path.as_ref();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn seed_cache(state: &State, info: &VersionInfo) {
        write(
            state
                .directories
                .version_dir(&info.id)
                .join(format!("{}.jar", info.id)),
            b"client jar",
        );
        for library in info
            .libraries
            .iter()
            .filter(|library| library.include_in_classpath)
        {
            write(
                state.directories.libraries_dir().join(
                    daedalus::get_path_from_artifact(&library.name).unwrap(),
                ),
                library.name.as_bytes(),
            );
        }
    }

    async fn finalize(
        state: &State,
        direct: &DirectLinkedLaunch,
        info: &VersionInfo,
        token: &CancellationToken,
    ) -> crate::Result<()> {
        finalize_version(
            direct,
            &info.id,
            "1.20.1",
            info,
            state,
            std::env::consts::ARCH,
            true,
            None,
            token,
        )
        .await
    }

    #[test]
    fn projection_keeps_portable_runtime_and_native_metadata() {
        let mut info = version_info();
        info.libraries = runtime_libraries();
        info.libraries.push(serde_json::from_value(json!({
            "name": "example:natives:1", "include_in_classpath": false,
            "natives": {"windows": "natives-windows"},
            "extract": {"exclude": ["META-INF/"]},
            "rules": [{"action": "allow", "os": {"name": "windows"}}],
            "downloads": {
                "artifact": {"path": "processor-only.jar", "sha1": "", "size": 0, "url": ""},
                "classifiers": {"natives-windows": {"path": "native.jar", "sha1": "", "size": 0, "url": ""}}
            }
        })).unwrap());
        let before = serde_json::to_value(&info).unwrap();
        let portable = project_manifest(&info, "Survival", "24w14a").unwrap();
        let document = &portable.document;
        assert_eq!(document["id"], "Survival");
        assert_eq!(document["clientVersion"], "24w14a");
        assert_eq!(document["releaseTime"], before["releaseTime"]);
        assert_eq!(document["time"], before["time"]);
        assert_eq!(document["arguments"], before["arguments"]);
        assert_eq!(document["mainClass"], before["mainClass"]);
        assert!(document.get("data").is_none());
        assert!(document.get("processors").is_none());
        let libraries = document["libraries"].as_array().unwrap();
        assert_eq!(libraries.len(), 4);
        for library in libraries {
            assert!(library.get("include_in_classpath").is_none());
            assert!(library.get("downloadable").is_none());
        }
        assert!(!portable.libraries[0].library.downloadable);
        assert!(!portable.libraries[3].library.include_in_classpath);
        assert!(libraries[3]["downloads"].get("artifact").is_none());
        assert_eq!(libraries[3]["natives"]["windows"], "natives-windows");
        assert_eq!(
            libraries[3]["downloads"]["classifiers"]["natives-windows"]["path"],
            "native.jar"
        );
        assert_eq!(libraries[3]["extract"]["exclude"][0], "META-INF/");
        assert_eq!(libraries[3]["rules"][0]["os"]["name"], "windows");
        assert_eq!(serde_json::to_value(&info).unwrap(), before);
    }

    #[tokio::test]
    async fn generated_forge_and_optifine_artifacts_resolve_after_idempotent_repair()
     {
        let (_temp, state, mut direct) = fixture().await;
        let mut info = version_info();
        info.libraries = runtime_libraries();
        seed_cache(&state, &info);
        write(
            direct.libraries_dir().join("custom/forge-client.jar"),
            b"obsolete forge",
        );
        write(
            direct.libraries_dir().join("unrelated/shared.jar"),
            b"unrelated",
        );
        write(direct.version_dir().join("saves/world/level.dat"), b"world");
        for mode in [ExternalGameDirMode::Isolated, ExternalGameDirMode::Shared]
        {
            direct.game_dir_mode = Some(mode);
            for _ in 0..2 {
                finalize(&state, &direct, &info, &CancellationToken::new())
                    .await
                    .unwrap();
                let resolved = direct.resolve().unwrap();
                let classpath = super::super::args::get_linked_class_paths(
                    &direct,
                    &resolved.merged.libraries,
                    &[&direct.client_jar("Survival")],
                    std::env::consts::ARCH,
                    true,
                )
                .unwrap();
                assert!(classpath.contains("forge-client.jar"), "{classpath}");
                assert!(
                    classpath.contains("OptiFine-1.20.1_HD_U_I6.jar"),
                    "{classpath}"
                );
                assert!(!classpath.contains("processor"), "{classpath}");
                assert_eq!(
                    std::fs::read(
                        direct.libraries_dir().join("custom/forge-client.jar")
                    )
                    .unwrap(),
                    info.libraries[0].name.as_bytes()
                );
                assert_eq!(
                    std::fs::read(direct.client_jar("Survival")).unwrap(),
                    b"client jar"
                );
            }
        }
        assert_eq!(
            std::fs::read(direct.libraries_dir().join("unrelated/shared.jar"))
                .unwrap(),
            b"unrelated"
        );
        assert_eq!(
            std::fs::read(direct.version_dir().join("saves/world/level.dat"))
                .unwrap(),
            b"world"
        );
        assert!(!direct.libraries_dir().join("processor").exists());
    }

    #[tokio::test]
    async fn missing_generated_output_preserves_previous_manifest() {
        let (_temp, state, direct) = fixture().await;
        let mut info = version_info();
        info.libraries = runtime_libraries();
        let json = direct.version_dir().join("Survival.json");
        write(&json, b"previous manifest");
        let error = finalize(&state, &direct, &info, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&info.libraries[0].name), "{error}");
        assert!(error.contains("custom"), "{error}");
        assert_eq!(std::fs::read(json).unwrap(), b"previous manifest");
        assert!(!direct.client_jar("Survival").exists());
    }

    #[tokio::test]
    async fn copy_failure_preserves_previous_manifest() {
        let (_temp, state, direct) = fixture().await;
        let mut info = version_info();
        info.libraries = runtime_libraries();
        seed_cache(&state, &info);
        let json = direct.version_dir().join("Survival.json");
        write(&json, b"previous manifest");
        write(direct.libraries_dir(), b"not a directory");
        let error = finalize(&state, &direct, &info, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("forge"), "{error}");
        assert_eq!(std::fs::read(json).unwrap(), b"previous manifest");
    }

    #[tokio::test]
    async fn cancellation_while_waiting_to_publish_preserves_previous_manifest()
    {
        let (_temp, state, direct) = fixture().await;
        let info = version_info();
        seed_cache(&state, &info);
        let json = direct.version_dir().join("Survival.json");
        write(&json, b"previous manifest");
        let lock = fetch::destination_download_lock(&json);
        let guard = lock.lock().await;
        let token = CancellationToken::new();
        let finalization = finalize(&state, &direct, &info, &token);
        let cancel = async {
            for _ in 0..100 {
                if direct.client_jar("Survival").is_file() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            token.cancel();
            drop(guard);
        };
        let (result, ()) = tokio::join!(finalization, cancel);
        assert!(result.unwrap_err().to_string().contains("canceled"));
        assert_eq!(std::fs::read(json).unwrap(), b"previous manifest");
    }

    #[tokio::test]
    async fn required_native_is_copied_and_missing_native_blocks_publication() {
        let (_temp, state, direct) = fixture().await;
        let mut info = version_info();
        let os =
            serde_json::to_value(daedalus::minecraft::Os::native().get_os())
                .unwrap();
        let classifier = format!("natives-{}", os.as_str().unwrap());
        info.libraries = serde_json::from_value(json!([{
            "name": "example:natives:1", "include_in_classpath": false, "downloadable": false,
            "natives": {os.as_str().unwrap(): classifier},
            "downloads": {"classifiers": {
                classifier: {"path": "native/current.jar", "sha1": "", "size": 0, "url": ""},
                "natives-other": {"path": "native/other.jar", "sha1": "", "size": 0, "url": ""}
            }}
        }])).unwrap();
        seed_cache(&state, &info);
        let error = finalize(&state, &direct, &info, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("native/current.jar")
                || error.contains("native\\current.jar"),
            "{error}"
        );
        assert!(!direct.version_dir().join("Survival.json").exists());
        write(
            state.directories.libraries_dir().join("native/current.jar"),
            b"current native",
        );
        write(
            state.directories.libraries_dir().join("native/other.jar"),
            b"other native",
        );
        finalize(&state, &direct, &info, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(direct.libraries_dir().join("native/current.jar"))
                .unwrap(),
            b"current native"
        );
        assert_eq!(
            std::fs::read(direct.libraries_dir().join("native/other.jar"))
                .unwrap(),
            b"other native"
        );
        let resolved = direct.resolve().unwrap();
        let classpath = super::super::args::get_linked_class_paths(
            &direct,
            &resolved.merged.libraries,
            &[&direct.client_jar("Survival")],
            std::env::consts::ARCH,
            true,
        )
        .unwrap();
        assert!(!classpath.contains("native"), "{classpath}");
    }
}
