use rmcp::{
    ErrorData,
    model::{Tool, ToolAnnotations},
};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Public,
    Session,
    FullAccess,
}

pub(crate) struct Entry {
    pub(crate) tool: Tool,
    pub(crate) access: Access,
    pub(crate) read_only: bool,
}

fn entry(
    name: &'static str,
    description: &'static str,
    access: Access,
    read_only: bool,
    idempotent: bool,
    properties: Value,
    required: &[&str],
) -> Entry {
    let mut properties = properties.as_object().unwrap().clone();
    let mut required: Vec<_> =
        required.iter().map(|name| json!(name)).collect();
    if access != Access::Public {
        properties.insert("session_id".into(), text(128));
        required.push(json!("session_id"));
    }
    let schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    Entry {
        tool: Tool::new(name, description, schema.as_object().unwrap().clone())
            .with_annotations(
                ToolAnnotations::new()
                    .read_only(read_only)
                    .destructive(access == Access::FullAccess)
                    .idempotent(idempotent)
                    .open_world(name == "instance_create"),
            ),
        access,
        read_only,
    }
}

fn text(max: u64) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max})
}
fn version() -> Value {
    json!({"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9][A-Za-z0-9._+\\-]*$"})
}
fn page() -> Value {
    json!({"offset":{"type":"integer","minimum":0,"maximum":4294967295_u64},"limit":{"type":"integer","minimum":1,"maximum":100}})
}

pub(crate) fn entries() -> Vec<Entry> {
    use Access::*;
    let id = text(256);
    let name = text(256);
    let mut list = page();
    list["search"] = json!({"type":"string","maxLength":256});
    let mut jobs = page();
    jobs["include_finished"] = json!({"type":"boolean"});
    vec![
        entry(
            "get_launcher_info",
            "Read launcher name, actual version and platform. No control approval needed.",
            Public,
            true,
            true,
            json!({}),
            &[],
        ),
        entry(
            "get_launcher_capabilities",
            "Discover implemented tools, session requirements and restrictions. No control approval needed. Only listed tools are implemented.",
            Public,
            true,
            true,
            json!({}),
            &[],
        ),
        entry(
            "start_session",
            "Request launcher control for the user's task. May show approval; never call during startup or discovery. Returns session_id required by business tools. Call end_session when finished.",
            Public,
            false,
            true,
            json!({}),
            &[],
        ),
        entry(
            "end_session",
            "Release control and remove the AI indicator. Keeps MCP transport available. Already admitted mutations finish; queued operations are cancelled. Installation jobs already created continue independently.",
            Session,
            false,
            true,
            json!({}),
            &[],
        ),
        entry(
            "instance_list",
            "List instance summaries with stable ID ordering, optional case-insensitive name search and pagination. Requires an approved control session.",
            Session,
            true,
            true,
            list,
            &[],
        ),
        entry(
            "instance_get",
            "Read one instance's game version, loader, installation state, groups, paths and playtime. Does not expose launch hooks, credentials or environment variables.",
            Session,
            true,
            true,
            json!({"instance_id":id}),
            &["instance_id"],
        ),
        entry(
            "instance_create",
            "Queue installation of a new unmanaged instance in the launcher directory. Returns job_id; poll install_job_get. Loader version may be omitted to use runtime defaults. Retrying creates another instance; do not retry blindly.",
            Session,
            false,
            false,
            json!({"name":name,"game_version":version(),"loader":{"type":"string","enum":["vanilla","fabric","forge","quilt","neoforge"]},"loader_version":version()}),
            &["name", "game_version", "loader"],
        ),
        entry(
            "instance_rename",
            "Rename an existing instance using its stable instance_id.",
            Session,
            false,
            true,
            json!({"instance_id":id,"name":name}),
            &["instance_id", "name"],
        ),
        entry(
            "instance_set_pinned",
            "Set or clear an instance's pinned state.",
            Session,
            false,
            true,
            json!({"instance_id":id,"pinned":{"type":"boolean"}}),
            &["instance_id", "pinned"],
        ),
        entry(
            "instance_group_list",
            "List instance groups and their stable IDs.",
            Session,
            true,
            true,
            json!({}),
            &[],
        ),
        entry(
            "instance_group_create",
            "Create an instance group. Retrying creates another group; do not retry blindly.",
            Session,
            false,
            false,
            json!({"name":name}),
            &["name"],
        ),
        entry(
            "instance_group_rename",
            "Rename an instance group. The built-in Favorites group cannot be renamed.",
            Session,
            false,
            true,
            json!({"group_id":id,"name":name}),
            &["group_id", "name"],
        ),
        entry(
            "instance_group_delete",
            "Delete a group and its membership associations, leaving instances intact. Requires full access. The built-in Favorites group cannot be deleted.",
            FullAccess,
            false,
            false,
            json!({"group_id":id}),
            &["group_id"],
        ),
        entry(
            "instance_set_groups",
            "Replace an instance's group memberships with the supplied group IDs. Empty array clears memberships; does not delete groups or instances.",
            Session,
            false,
            true,
            json!({"instance_id":id,"group_ids":{"type":"array","maxItems":100,"items":id}}),
            &["instance_id", "group_ids"],
        ),
        entry(
            "process_list",
            "List launcher-managed game processes, optionally filtered by instance_id. Does not expose process command lines or environment variables.",
            Session,
            true,
            true,
            json!({"instance_id":id}),
            &[],
        ),
        entry(
            "java_list",
            "List registered Java runtimes and default versions. Does not scan the filesystem or execute Java.",
            Session,
            true,
            true,
            json!({}),
            &[],
        ),
        entry(
            "system_get_memory",
            "Read system total and available memory in bytes. Does not run memory optimization.",
            Session,
            true,
            true,
            json!({}),
            &[],
        ),
        entry(
            "install_job_list",
            "List installation job summaries, newest first, with pagination. Download URLs and raw diagnostics are omitted.",
            Session,
            true,
            true,
            jobs,
            &[],
        ),
        entry(
            "install_job_get",
            "Read an installation job summary by job_id. Includes progress and error code, without raw diagnostics or download URLs.",
            Session,
            true,
            true,
            json!({"job_id":{"type":"string","format":"uuid"}}),
            &["job_id"],
        ),
    ]
}

pub(crate) fn definitions() -> Vec<Tool> {
    entries().into_iter().map(|entry| entry.tool).collect()
}

// The catalog uses only these scalar/array constraints. Validate them before
// typed dispatch so advertised bounds also apply to raw JSON-RPC callers.
pub(crate) fn validate(
    entry: &Entry,
    arguments: &Map<String, Value>,
) -> Result<(), ErrorData> {
    let schema = &entry.tool.input_schema;
    let properties = schema["properties"].as_object().unwrap();
    for required in schema["required"].as_array().unwrap() {
        if !arguments.contains_key(required.as_str().unwrap()) {
            return Err(ErrorData::invalid_params(
                format!("Missing argument: {required}"),
                None,
            ));
        }
    }
    for (name, value) in arguments {
        let valid = properties
            .get(name)
            .is_some_and(|rule| valid_value(value, rule));
        if !valid {
            return Err(ErrorData::invalid_params(
                format!("Invalid or unknown argument: {name}"),
                None,
            ));
        }
    }
    Ok(())
}

fn valid_value(value: &Value, rule: &Value) -> bool {
    match rule["type"].as_str() {
        Some("string") => value.as_str().is_some_and(|s| {
            let len = s.chars().count() as u64;
            !s.chars().any(char::is_control)
                && rule["minLength"]
                    .as_u64()
                    .is_none_or(|min| len >= min && !s.trim().is_empty())
                && rule["maxLength"].as_u64().is_none_or(|max| len <= max)
                && rule["enum"]
                    .as_array()
                    .is_none_or(|choices| choices.contains(value))
                && rule["pattern"].as_str().is_none_or(|pattern| {
                    pattern == "^[A-Za-z0-9][A-Za-z0-9._+\\-]*$"
                        && s.as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && s.bytes().all(|c| {
                            c.is_ascii_alphanumeric()
                                || matches!(c, b'.' | b'_' | b'+' | b'-')
                        })
                })
                && (rule["format"] != "uuid"
                    || uuid::Uuid::parse_str(s).is_ok())
        }),
        Some("integer") => value.as_u64().is_some_and(|n| {
            rule["minimum"].as_u64().is_none_or(|min| n >= min)
                && rule["maximum"].as_u64().is_none_or(|max| n <= max)
        }),
        Some("boolean") => value.is_boolean(),
        Some("array") => value.as_array().is_some_and(|items| {
            rule["maxItems"]
                .as_u64()
                .is_none_or(|max| items.len() as u64 <= max)
                && items.iter().all(|item| valid_value(item, &rule["items"]))
        }),
        _ => false,
    }
}
