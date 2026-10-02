use std::path::PathBuf;

use crate::{
	event::{
		CommandPayload,
		emit::{emit_command, emit_warning},
	},
	state::Settings,
	util::io,
};
use url::form_urlencoded;
use urlencoding::decode as url_decode;

fn query_map(query: &str) -> std::collections::HashMap<String, String> {
	let mut map = std::collections::HashMap::new();
	for (key, value) in form_urlencoded::parse(query.as_bytes()) {
		map.entry(key.into_owned()).or_insert(value.into_owned());
	}
	map
}

fn non_empty(value: Option<String>) -> Option<String> {
	value.filter(|v| !v.trim().is_empty())
}

fn launch_parts(
	query: &str,
) -> (Option<String>, Option<String>, Option<String>) {
	let map = query_map(query);
	(
		non_empty(map.get("instance_id").cloned()),
		non_empty(map.get("server").cloned()),
		non_empty(map.get("singleplayer_world").cloned()),
	)
}

async fn launch_payload(
	id: Option<String>,
	server: Option<String>,
	singleplayer_world: Option<String>,
) -> crate::Result<CommandPayload> {
	if server.is_some() && singleplayer_world.is_some() {
		emit_warning("Invalid command, cannot launch both a server and a singleplayer world").await?;
		return Err(crate::ErrorKind::InputError(
			"Cannot launch both a server and a singleplayer world".to_string(),
		)
		.into());
	}
	match id {
		Some(id) => Ok(CommandPayload::LaunchInstance {
			id,
			server,
			singleplayer_world,
		}),
		None => Err(crate::ErrorKind::InputError(
			"Launch command requires an instance_id query parameter".to_string(),
		)
		.into()),
	}
}

async fn unknown_path(sublink: &str) -> crate::Result<CommandPayload> {
	emit_warning(&format!("Invalid command, unrecognized path: {sublink}")).await?;
	Err(crate::ErrorKind::InputError(format!(
		"Invalid command, unrecognized path: {sublink}"
	))
	.into())
}

fn valid_project_type(value: &str) -> bool {
	matches!(
		value,
		"mod" | "modpack"
			| "resourcepack"
			| "datapack" | "shader"
			| "plugin" | "server"
	)
}

fn valid_lab_tool(value: &str) -> bool {
	matches!(
		value,
		"skin-editor"
			| "gradient-text" | "recipe-generator"
			| "schematic-preview" | "mod-translation"
	)
}

fn open_route(
	path: String,
	query: Option<&str>,
) -> crate::Result<CommandPayload> {
	let query = query
		.map(str::trim)
		.filter(|q| !q.is_empty())
		.map(str::to_string);
	Ok(CommandPayload::OpenRoute { path, query })
}

fn split_path_query(sublink: &str) -> (&str, &str) {
	sublink.split_once('?').unwrap_or((sublink, ""))
}

fn decode_segment(raw: &str) -> Option<String> {
	match url_decode(raw) {
		Ok(v) => {
			let v = v.to_string();
			if v.trim().is_empty() { None } else { Some(v) }
		}
		Err(_) => None,
	}
}

pub async fn handle_url(sublink: &str) -> crate::Result<CommandPayload> {
	let sublink = sublink.trim().trim_start_matches('/');
	if sublink.is_empty() {
		return Ok(CommandPayload::OpenRoute {
			path: "/".to_string(),
			query: None,
		});
	}
	if let Some(rest) = sublink.strip_prefix("open?").or_else(|| sublink.strip_prefix("open/")) {
		let rest = rest.trim_start_matches('/').trim_start_matches('?');
		let map = query_map(rest);
		if let Some(path) = non_empty(map.get("path").cloned()) {
			let extra: Vec<String> = form_urlencoded::parse(rest.as_bytes())
				.filter(|(k, _)| k != "path")
				.map(|(k, v)| format!("{k}={v}"))
				.collect();
			let query = (!extra.is_empty()).then(|| extra.join("&"));
			return open_route(format!("/{}", path.trim_start_matches('/')), query.as_deref());
		}
		return unknown_path(sublink).await;
	}
	if sublink == "home" {
		return open_route("/".to_string(), None);
	}
	if sublink == "discovery" {
		return Ok(CommandPayload::OpenDiscovery);
	}
	if sublink == "favorites" {
		return open_route("/browse/favorites".to_string(), None);
	}
	if let Some(query) = sublink.strip_prefix("join?") {
		let map = query_map(query);
		let id = non_empty(map.get("instance_id").cloned());
		let server = non_empty(map.get("server").cloned());
		let world = non_empty(map.get("singleplayer_world").cloned());
		if server.is_none() && world.is_none() {
			return Err(crate::ErrorKind::InputError(
				"Join command requires a server or singleplayer_world query parameter".to_string(),
			)
			.into());
		}
		return launch_payload(id, server, world).await;
	}
	if let Some(query) = sublink.strip_prefix("launch?") {
		let (id, server, world) = launch_parts(query);
		return launch_payload(id, server, world).await;
	}
	if let Some(query) = sublink.strip_prefix("install?") {
		let map = query_map(query);
		let project = non_empty(map.get("project").cloned());
		let version = non_empty(map.get("version").cloned());
		match (project, version) {
			(_, Some(version)) => {
				return Ok(CommandPayload::InstallVersion { id: version });
			}
			(Some(project), None) => {
				let kind = map.get("kind").map(String::as_str).unwrap_or("mod");
				return match kind {
					"modpack" => Ok(CommandPayload::InstallModpack { id: project }),
					"server" => Ok(CommandPayload::InstallServer { id: project }),
					_ => Ok(CommandPayload::InstallMod { id: project }),
				};
			}
			_ => {
				return Err(crate::ErrorKind::InputError(
					"Install command requires a project or version query parameter".to_string(),
				)
				.into());
			}
		}
	}
	if let Some(rest) = sublink.strip_prefix("seed-map")
		&& (rest.is_empty() || rest.starts_with('?') || rest.starts_with('/'))
	{
		let query = rest.trim_start_matches('/').trim_start_matches('?');
		return Ok(CommandPayload::OpenSeedMap {
			query: query.to_string(),
		});
	}
	if let Some(rest) = sublink.strip_prefix("settings") {
		let query = rest.trim_start_matches('/').trim_start_matches('?');
		let map = query_map(query);
		return Ok(CommandPayload::OpenSettings {
			tab: non_empty(map.get("tab").cloned()),
			entry: non_empty(map.get("entry").cloned()),
		});
	}
	let (path_part, query_part) = split_path_query(sublink);
	let mut segments = path_part.split('/').filter(|s| !s.is_empty());
	let namespace = segments.next().unwrap_or("");
	let first = segments.next().unwrap_or("");
	let second = segments.next().unwrap_or("");
	let extra = segments.next().is_some();
	match namespace {
		"launch" if first == "instance" && !extra => {
			let map = query_map(query_part);
			let id = decode_segment(second);
			let server = non_empty(map.get("server").cloned());
			let world = non_empty(map.get("singleplayer_world").cloned());
			return launch_payload(id, server, world).await;
		}
		"join" if !extra => {
			let id = decode_segment(first);
			let map = query_map(query_part);
			let server = non_empty(map.get("server").cloned());
			let world = non_empty(map.get("singleplayer_world").cloned());
			if server.is_none() && world.is_none() {
				return Err(crate::ErrorKind::InputError(
					"Join command requires a server or singleplayer_world query parameter".to_string(),
				)
				.into());
			}
			return launch_payload(id, server, world).await;
		}
		"project" if !first.is_empty() && !extra => {
			let id = decoded(first);
			let map = query_map(query_part);
			let tab = map.get("tab").map(String::as_str).unwrap_or("");
			let version = non_empty(map.get("version").cloned());
			if let Some(version) = version {
				return Ok(CommandPayload::InstallVersion { id: version });
			}
			let suffix = match tab {
				"versions" => "/versions".to_string(),
				"gallery" => "/gallery".to_string(),
				"changelog" => "/changelog".to_string(),
				_ => String::new(),
			};
			return open_route(format!("/project/{id}{suffix}"), (!query_part.is_empty()).then_some(query_part));
		}
		"browse" if valid_project_type(first) && !extra => {
			return open_route(
				format!("/browse/{first}"),
				(!query_part.is_empty()).then_some(query_part),
			);
		}
		"library" if ["", "downloaded", "modpacks", "servers", "custom"].contains(&first) && !extra => {
			let path = if first.is_empty() {
				"/library".to_string()
			} else {
				format!("/library/{first}")
			};
			return open_route(path, (!query_part.is_empty()).then_some(query_part));
		}
		"instance" if !first.is_empty() && !extra => {
			let id = decoded(first);
			let map = query_map(query_part);
			let tab = map.get("tab").map(String::as_str).unwrap_or("");
			let path = match tab {
				"mods" | "" => format!("/instance/{id}"),
				"files" => format!("/instance/{id}/files"),
				"studio" => format!("/instance/{id}/files/studio"),
				"logs" => format!("/instance/{id}/logs"),
				"worlds" => format!("/instance/{id}/worlds"),
				"screenshots" => format!("/instance/{id}/screenshots"),
				"upgrade" => format!("/instance/{id}/upgrade"),
				_ => return unknown_path(sublink).await,
			};
			return open_route(path, None);
		}
		"downloads" if first.is_empty() && !extra => {
			return open_route(
				"/downloads".to_string(),
				(!query_part.is_empty()).then_some(query_part),
			);
		}
		"create" if first.is_empty() && !extra => {
			return open_route(
				"/create".to_string(),
				(!query_part.is_empty()).then_some(query_part),
			);
		}
		"skins" | "worlds" | "screenshots" | "favorites" if first.is_empty() && !extra => {
			return open_route(
				format!("/{namespace}"),
				(!query_part.is_empty()).then_some(query_part),
			);
		}
		"multiplayer" if ["", "servers", "rooms"].contains(&first) && (second.is_empty() || first == "servers") && !extra => {
			let path = if first.is_empty() {
				"/multiplayer/servers".to_string()
			} else if first == "rooms" {
				"/multiplayer/rooms".to_string()
			} else if second.is_empty() {
				"/multiplayer/servers".to_string()
			} else {
				format!("/multiplayer/servers/{second}")
			};
			return open_route(path, (!query_part.is_empty()).then_some(query_part));
		}
		"lab" if (first.is_empty() || first == "seed-map" || valid_lab_tool(first)) && second.is_empty() && !extra => {
			if first == "seed-map" {
				return Ok(CommandPayload::OpenSeedMap {
					query: query_part.to_string(),
				});
			}
			let path = if first.is_empty() {
				"/lab".to_string()
			} else {
				format!("/lab/{first}")
			};
			return open_route(path, (!query_part.is_empty()).then_some(query_part));
		}
		"help" if first == "drop" && second.is_empty() && !extra => {
			return open_route("/help/drop".to_string(), None);
		}
		"mod" if !first.is_empty() && second.is_empty() && !extra => {
			return Ok(CommandPayload::InstallMod { id: first.to_string() });
		}
		"version" if !first.is_empty() && second.is_empty() && !extra => {
			return Ok(CommandPayload::InstallVersion { id: first.to_string() });
		}
		"modpack" if !first.is_empty() && second.is_empty() && !extra => {
			return Ok(CommandPayload::InstallModpack { id: first.to_string() });
		}
		"server" if !first.is_empty() && second.is_empty() && !extra => {
			return Ok(CommandPayload::InstallServer { id: first.to_string() });
		}
		_ => {}
	}
	unknown_path(sublink).await
}

fn decoded(raw: &str) -> String {
	decode_segment(raw).unwrap_or_default()
}

async fn external_scheme_allowed() -> bool {
	match crate::State::get().await {
		Ok(state) => match Settings::get(&state.pool).await {
			Ok(settings) => settings.allow_external_scheme,
			Err(_) => true,
		},
		Err(_) => true,
	}
}

pub async fn parse_command(
	command_string: &str,
) -> crate::Result<CommandPayload> {
	tracing::debug!("Parsing command: {}", &command_string);
	if let Some(sublink) = command_string.strip_prefix("axolotl://") {
		if !external_scheme_allowed().await {
			emit_warning("External links are disabled in settings").await?;
			return Err(crate::ErrorKind::InputError(
				"External links are disabled in settings".to_string(),
			)
			.into());
		}
		Ok(handle_url(sublink).await?)
	} else {
		let path = PathBuf::from(command_string);
		let path = io::canonicalize(path)?;
		if let Some(ext) = path.extension()
			&& (ext == "mrpack" || ext == "zip")
		{
			return Ok(CommandPayload::RunMRPack { path });
		}
		emit_warning(&format!(
			"Invalid command, unrecognized filetype: {}",
			path.display()
		))
		.await?;
		Err(crate::ErrorKind::InputError(format!(
			"Invalid command, unrecognized filetype: {}",
			path.display()
		))
		.into())
	}
}

pub async fn parse_and_emit_command(command_string: &str) -> crate::Result<()> {
	let command = parse_command(command_string).await?;
	emit_command(command).await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn parses_launch_query_command() {
		let command =
			parse_command(
				"axolotl://launch?instance_id=example%20instance&server=example.org%3A25565",
			)
			.await
			.unwrap();
		assert!(matches!(
			command,
			CommandPayload::LaunchInstance { id, server: Some(server), singleplayer_world: None }
				if id == "example instance" && server == "example.org:25565"
		));
	}

	#[tokio::test]
	async fn parses_discovery_command() {
		assert!(matches!(
			parse_command("axolotl://discovery").await.unwrap(),
			CommandPayload::OpenDiscovery
		));
	}

	#[tokio::test]
	async fn parses_unified_commands() {
		assert!(matches!(
			parse_command("axolotl://project/sodium?tab=versions").await.unwrap(),
			CommandPayload::OpenRoute { path, .. } if path == "/project/sodium/versions"
		));
		assert!(matches!(
			parse_command("axolotl://browse/mod?q=carpet").await.unwrap(),
			CommandPayload::OpenRoute { path, query } if path == "/browse/mod" && query == Some("q=carpet".to_string())
		));
		assert!(matches!(
			parse_command("axolotl://instance/my-pack?tab=logs").await.unwrap(),
			CommandPayload::OpenRoute { path, .. } if path == "/instance/my-pack/logs"
		));
		assert!(matches!(
			parse_command("axolotl://settings?tab=privacy-data").await.unwrap(),
			CommandPayload::OpenSettings { tab: Some(tab), .. } if tab == "privacy-data"
		));
		assert!(matches!(
			parse_command("axolotl://open?path=/library/servers").await.unwrap(),
			CommandPayload::OpenRoute { path, .. } if path == "/library/servers"
		));
		assert!(matches!(
			parse_command("axolotl://install?version=abc123").await.unwrap(),
			CommandPayload::InstallVersion { id } if id == "abc123"
		));
	}
}
