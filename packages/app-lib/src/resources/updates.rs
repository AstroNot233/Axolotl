use super::*;
use futures::{StreamExt, stream};
use tokio_util::sync::CancellationToken;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUpdateCheck {
    pub entries: Vec<InstalledEntryId>,
    pub releases: std::collections::HashMap<InstalledEntryId, SourceReleaseRef>,
    pub unresolved: Vec<String>,
    pub failures: Vec<String>,
    pub failed_entries: Vec<InstalledEntryId>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUpdateProgress {
    pub entry_id: InstalledEntryId,
    pub release: Option<SourceReleaseRef>,
    pub failed: bool,
    pub unresolved: bool,
}

impl ResourceUpdateCheck {
    fn progress_for(
        &self,
        entry_id: &InstalledEntryId,
        unresolved: bool,
    ) -> ResourceUpdateProgress {
        ResourceUpdateProgress {
            entry_id: entry_id.clone(),
            release: self.releases.get(entry_id).cloned(),
            failed: self.failed_entries.contains(entry_id),
            unresolved,
        }
    }
}

pub async fn check(
    instance_id: String,
    entry_ids: Option<&[InstalledEntryId]>,
    on_progress: impl Fn(ResourceUpdateProgress) + Send + Sync,
    cancel: &CancellationToken,
) -> ResourceResult<ResourceUpdateCheck> {
    let state = crate::State::get().await?;
    let service = state.resources().await?;
    let all_ids;
    let ids = match entry_ids {
        Some(ids) => ids,
        None => {
            all_ids = plans::installed(&state.pool, &instance_id)
                .await?
                .into_iter()
                .map(|entry| entry.entry_id)
                .collect::<Vec<_>>();
            &all_ids
        }
    };
    // Discover against the displayed content set. A full planning hydration
    // would rescan files and resolve retained dependencies before any result.
    let hydrated = installed::hydrate_update_batch_with_service(
        &state,
        &instance_id,
        cancel,
        ids,
        service,
    )
    .await?;
    let instance = crate::state::get_instance(&instance_id, &state.pool)
        .await?
        .ok_or_else(|| {
            ResourceError::InvalidRequest("unknown instance".into())
        })?;
    let target = ipc::ResourceTarget {
        game_versions: vec![instance.applied_content_set.game_version],
        loaders: vec![instance.applied_content_set.loader.as_str().into()],
        channels: plans::update_channels(instance.instance.update_channel),
    };
    let preferred = service.preferred_source().await?;
    let result = ResourceUpdateCheck {
        entries: vec![],
        releases: std::collections::HashMap::new(),
        unresolved: vec![],
        failures: hydrated
            .failures
            .iter()
            .map(|(_, error)| error.clone())
            .collect(),
        failed_entries: hydrated
            .failures
            .iter()
            .map(|(id, _)| id.clone())
            .collect(),
    };
    for entry_id in &result.failed_entries {
        on_progress(result.progress_for(entry_id, false));
    }
    // Discovery reports newer compatible releases. Dependency graph and exact
    // candidate JAR validation belong to the reviewed apply plan, where a
    // failure can be handled without suppressing every update in the instance.
    check_entries(
        service,
        hydrated.entries,
        result,
        &target,
        &preferred,
        on_progress,
        cancel,
    )
    .await
}

pub(crate) async fn check_entries(
    service: &std::sync::Arc<ResourceService>,
    entries: Vec<InstalledResource>,
    mut result: ResourceUpdateCheck,
    target: &ipc::ResourceTarget,
    preferred: &SourceId,
    on_progress: impl Fn(ResourceUpdateProgress) + Send + Sync,
    cancel: &CancellationToken,
) -> ResourceResult<ResourceUpdateCheck> {
    let failed = &result.failed_entries;
    let entries = entries
        .into_iter()
        .filter(|entry| !entry.missing && !failed.contains(&entry.entry_id))
        .collect::<Vec<_>>();
    let mut checks = stream::iter(entries)
        .map(|entry| async move {
            let outcome =
                check_entry(service, &entry, target, preferred, cancel).await;
            (entry, outcome)
        })
        .buffer_unordered(8);
    while let Some((entry, outcome)) = checks.next().await {
        if cancel.is_cancelled() {
            return Err(ResourceError::Canceled);
        }
        let unresolved = entry.artifact.is_none();
        match outcome {
            Ok(Some(release)) => {
                result.releases.insert(entry.entry_id.clone(), release);
                result.entries.push(entry.entry_id.clone());
            }
            Ok(None) => {
                if unresolved {
                    result.unresolved.push(entry.relative_path.clone());
                }
            }
            Err(ResourceError::Canceled) => {
                return Err(ResourceError::Canceled);
            }
            Err(ResourceError::NoCompatibleRelease) => {}
            Err(error) => {
                result.failed_entries.push(entry.entry_id.clone());
                result
                    .failures
                    .push(format!("{}: {error}", entry.relative_path));
            }
        }
        on_progress(result.progress_for(&entry.entry_id, unresolved));
    }
    Ok(result)
}

async fn check_entry(
    service: &std::sync::Arc<ResourceService>,
    entry: &InstalledResource,
    target: &ipc::ResourceTarget,
    preferred: &SourceId,
    cancel: &CancellationToken,
) -> ResourceResult<Option<SourceReleaseRef>> {
    check_entry_with_policy(
        service,
        entry,
        target,
        preferred,
        cancel,
        CachePolicy::UseFresh,
    )
    .await
}

pub(crate) async fn check_entry_with_policy(
    service: &std::sync::Arc<ResourceService>,
    entry: &InstalledResource,
    target: &ipc::ResourceTarget,
    preferred: &SourceId,
    cancel: &CancellationToken,
    policy: CachePolicy,
) -> ResourceResult<Option<SourceReleaseRef>> {
    if cancel.is_cancelled() {
        return Err(ResourceError::Canceled);
    }
    let Some(artifact) = &entry.artifact else {
        return Ok(None);
    };
    let selection = ResourceSelection {
        id: SelectionId::new(entry.entry_id.as_str())?,
        resource: entry
            .resource_id
            .clone()
            .map(|id| ResourceRef::Resource { id })
            .unwrap_or_else(|| ResourceRef::Source {
                project: artifact.release.project.clone(),
            }),
        release: ReleaseSelection::Automatic,
        resource_type: entry.resource_type,
        scope: entry.scope.clone(),
        entry_id: Some(entry.entry_id.clone()),
    };
    let current = service
        .release_with_policy(&artifact.release, None, cancel, policy)
        .await?;
    let (_, projects) = service.source_projects(&selection.resource).await?;
    if projects.len() == 1
        && projects[0] == artifact.release.project
        && matches!(projects[0].source.as_str(), "modrinth" | "curseforge")
    {
        let mut catalog_target = target.clone();
        if matches!(
            entry.resource_type,
            ResourceType::ResourcePack
                | ResourceType::Shader
                | ResourceType::DataPack
                | ResourceType::World
        ) {
            catalog_target.loaders.clear();
        }
        if let Some(hash) = entry
            .integrity
            .hashes
            .iter()
            .find(|hash| hash.algorithm == HashAlgorithm::Sha1)
        {
            let response = service
                .query(
                    &projects[0].source,
                    providers::ProviderQuery::UpdateCandidates {
                        project_id: projects[0].project_id.clone(),
                        sha1: hash.value.clone(),
                        target: catalog_target,
                    },
                    policy,
                    cancel,
                )
                .await;
            match response {
                Ok(QueryOutcome {
                    value: providers::ProviderValue::UpdateCandidates(documents),
                    ..
                }) => {
                    let releases = documents
                        .into_iter()
                        .map(|document| document.release)
                        .filter(|release| {
                            let compatibility = assess_compatibility(
                                release,
                                target,
                                entry.resource_type,
                            );
                            compatibility.game_version
                                == CompatibilityState::Compatible
                                && compatibility.loader
                                    == CompatibilityState::Compatible
                                && (target.channels.is_empty()
                                    || target
                                        .channels
                                        .contains(&release.channel))
                                && release_is_newer(release, &current)
                        })
                        .collect::<Vec<_>>();
                    if let Some((release, _)) =
                        select_automatic_release(releases, preferred)
                    {
                        // Latest-file APIs order by publication. A numeric-only
                        // catalog uses version ordering, requiring its filtered list.
                        let numeric = release
                            .version
                            .strip_prefix('v')
                            .unwrap_or(&release.version)
                            .split('.')
                            .all(|part| {
                                !part.is_empty()
                                    && part
                                        .bytes()
                                        .all(|byte| byte.is_ascii_digit())
                                    && part.parse::<u64>().is_ok()
                            });
                        let primary = release
                            .artifacts
                            .iter()
                            .filter(|artifact| {
                                artifact.role == ArtifactRole::Primary
                            })
                            .collect::<Vec<_>>();
                        if !numeric
                            && let [candidate] = primary.as_slice()
                            && valid_artifact(candidate)
                            && candidate.integrity.size.is_some()
                            && candidate.available != Some(false)
                            && candidate.integrity.hashes.iter().any(|hash| {
                                hash.algorithm != HashAlgorithm::Md5
                            })
                            && !identity::exact_integrity(
                                &entry.integrity,
                                &candidate.integrity,
                            )
                        {
                            return Ok(Some(release.reference));
                        }
                    }
                }
                Err(ResourceError::Canceled) => {
                    return Err(ResourceError::Canceled);
                }
                _ => {}
            }
        }
    }
    let (release, artifact, _) = service
        .selected_release_with_requirements_policy(
            &selection,
            target,
            preferred,
            cancel,
            Some(&current),
            &[],
            &[],
            policy,
            true,
        )
        .await?;
    Ok(
        (!identity::exact_integrity(&entry.integrity, &artifact.integrity)
            && release_is_newer(&release, &current))
        .then_some(release.reference),
    )
}
