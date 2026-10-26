//! What the collections promise, exercised against a database of their own.
//!
//! Each collection answers for the verbs it inherits from `Collection`, so its
//! tests live here together instead of inside the module that writes the
//! statements: a change to a verb is answered once, in one place.

mod content_favorites;
mod game_option_locale_origins;
mod game_option_pack_bases;
mod game_option_update_state;
mod instance_sync_checkpoints;
mod instance_sync_preferences;
mod join_log;
mod official_login_proof;
mod resource_source_projects;
mod settings;
mod settings_api;
mod settings_db;
mod support;
mod sync_feature_settings;
mod synced_game_option_preferences;
mod synced_game_option_state;
mod synced_game_option_values;
