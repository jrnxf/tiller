//! The developer's Claude Code plugins that carry hooks stay off in every
//! session tiller launches (`memory.md` section 3, L6).
//!
//! A fleet pane reads the developer's own user settings, so every plugin
//! they enabled loads there too (P67 keeps their skills callable). A plugin
//! hook can put text into the session's context: a `SessionStart` hook that
//! prints saved notes would hand unapproved memory to an unwatched role.
//! So each launch lists every installed plugin that has hooks and names it
//! `false` under `enabledPlugins` in the run's settings fragment, which
//! Claude Code ranks above the user settings. A plugin without hooks keeps
//! its skills and commands.
//!
//! The list is read from the config home Claude Code reads it from:
//! `plugins/installed_plugins.json` names each plugin and its install
//! folder, and a plugin has hooks when any of these holds:
//!
//! - its folder has `hooks/hooks.json`;
//! - its manifest (`.claude-plugin/plugin.json`) has a `hooks` key, or
//!   cannot be read;
//! - its marketplace entry has a `hooks` key;
//! - it has no manifest and no marketplace entry tiller can read, so
//!   nothing says it has none.
//!
//! When in doubt a plugin is switched off. A plugin list that exists but
//! cannot be read refuses the launch, since tiller could not name what to
//! switch off.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::trust::ClaudeConfig;

/// The plugin list, under the config home.
const INSTALLED: &str = "plugins/installed_plugins.json";
/// The marketplace list, under the config home.
const MARKETPLACES: &str = "plugins/known_marketplaces.json";

/// The installed plugins that have hooks, by their `name@marketplace` key,
/// sorted. No plugin list means no plugins.
pub fn hooked(config: &ClaudeConfig) -> Result<Vec<String>, String> {
    let home = config.home()?;
    let list = home.join(INSTALLED);
    let Some(installed) = read_json(&list)? else {
        return Ok(Vec::new());
    };
    let plugins = installed
        .get("plugins")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{} has no `plugins` object", list.display()))?;
    let marketplaces = read_json(&home.join(MARKETPLACES))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let mut names = BTreeSet::new();
    for (key, installs) in plugins {
        let folders: Vec<&Path> = installs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|install| install.get("installPath")?.as_str())
            .map(Path::new)
            .collect();
        let entry = marketplace_entry(&marketplaces, key);
        if folders.is_empty()
            || folders
                .iter()
                .any(|folder| has_hooks(folder, entry.as_ref()))
        {
            names.insert(key.clone());
        }
    }
    Ok(names.into_iter().collect())
}

/// Names each of `plugins` `false` under the fragment's `enabledPlugins`.
pub fn switch_off(fragment: &mut Value, plugins: &[String]) {
    if plugins.is_empty() {
        return;
    }
    let Some(object) = fragment.as_object_mut() else {
        return;
    };
    let enabled = object
        .entry("enabledPlugins")
        .or_insert_with(|| Value::Object(Map::new()));
    if !enabled.is_object() {
        *enabled = Value::Object(Map::new());
    }
    if let Some(enabled) = enabled.as_object_mut() {
        for plugin in plugins {
            enabled.insert(plugin.clone(), Value::Bool(false));
        }
    }
}

/// Whether the plugin installed at `folder` has hooks, or might.
fn has_hooks(folder: &Path, entry: Option<&Value>) -> bool {
    if folder.join("hooks").join("hooks.json").exists() {
        return true;
    }
    if entry.is_some_and(|entry| entry.get("hooks").is_some()) {
        return true;
    }
    match read_json(&folder.join(".claude-plugin").join("plugin.json")) {
        Ok(Some(manifest)) => manifest.get("hooks").is_some(),
        Ok(None) => entry.is_none(),
        Err(_) => true,
    }
}

/// The marketplace entry of plugin `key` (`name@marketplace`).
fn marketplace_entry(marketplaces: &Value, key: &str) -> Option<Value> {
    let (name, marketplace) = key.rsplit_once('@')?;
    let location = marketplaces
        .get(marketplace)?
        .get("installLocation")?
        .as_str()?;
    let path = PathBuf::from(location)
        .join(".claude-plugin")
        .join("marketplace.json");
    let manifest = read_json(&path).ok()??;
    manifest
        .get("plugins")?
        .as_array()?
        .iter()
        .find(|plugin| plugin.get("name").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// The JSON at `path`, or `None` when there is no file. A path whose parent
/// is a file (not a folder) has no file either.
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{} is not JSON: {error}", path.display())),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Home {
        dir: tempfile::TempDir,
    }

    impl Home {
        fn new() -> Self {
            let home = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            fs::create_dir_all(home.dir.path().join("plugins")).unwrap();
            home
        }

        fn config(&self) -> ClaudeConfig {
            ClaudeConfig::at(self.dir.path())
        }

        fn write(&self, path: &Path, value: &Value) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
        }

        /// Installs `key` with an optional manifest and hook file, and
        /// returns its folder.
        fn plugin(&self, key: &str, manifest: Option<Value>, hooks: bool) -> PathBuf {
            let folder = self.dir.path().join("cache").join(key);
            fs::create_dir_all(&folder).unwrap();
            if let Some(manifest) = manifest {
                self.write(&folder.join(".claude-plugin/plugin.json"), &manifest);
            }
            if hooks {
                self.write(&folder.join("hooks/hooks.json"), &json!({"hooks": {}}));
            }
            folder
        }

        fn installed(&self, plugins: &[(&str, &Path)]) {
            let mut map = Map::new();
            for (key, folder) in plugins {
                map.insert(
                    (*key).to_owned(),
                    json!([{"scope": "user", "installPath": folder}]),
                );
            }
            self.write(
                &self.dir.path().join(INSTALLED),
                &json!({"version": 2, "plugins": map}),
            );
        }

        fn marketplace(&self, name: &str, plugins: Value) {
            let location = self.dir.path().join("marketplaces").join(name);
            self.write(
                &location.join(".claude-plugin/marketplace.json"),
                &json!({"name": name, "plugins": plugins}),
            );
            self.write(
                &self.dir.path().join(MARKETPLACES),
                &json!({name: {"installLocation": location}}),
            );
        }
    }

    #[test]
    fn a_plugin_with_hooks_is_switched_off_and_one_without_is_kept() {
        let home = Home::new();
        let notes = home.plugin("notes@shop", Some(json!({"name": "notes"})), true);
        let inline = home.plugin(
            "inline@shop",
            Some(json!({"name": "inline", "hooks": {"SessionStart": []}})),
            false,
        );
        let skills = home.plugin("skills@shop", Some(json!({"name": "skills"})), false);
        home.installed(&[
            ("notes@shop", &notes),
            ("inline@shop", &inline),
            ("skills@shop", &skills),
        ]);
        assert_eq!(
            hooked(&home.config()).unwrap(),
            ["inline@shop", "notes@shop"]
        );
    }

    #[test]
    fn a_plugin_nothing_describes_is_switched_off() {
        let home = Home::new();
        let bare = home.plugin("bare@shop", None, false);
        let broken = home.plugin("broken@shop", None, false);
        fs::create_dir_all(broken.join(".claude-plugin")).unwrap();
        fs::write(broken.join(".claude-plugin/plugin.json"), "{").unwrap();
        home.installed(&[("bare@shop", &bare), ("broken@shop", &broken)]);
        assert_eq!(
            hooked(&home.config()).unwrap(),
            ["bare@shop", "broken@shop"]
        );
    }

    #[test]
    fn the_marketplace_entry_counts() {
        let home = Home::new();
        let lsp = home.plugin("lsp@shop", None, false);
        let hooks = home.plugin("hooks@shop", Some(json!({"name": "hooks"})), false);
        home.installed(&[("lsp@shop", &lsp), ("hooks@shop", &hooks)]);
        home.marketplace(
            "shop",
            json!([
                {"name": "lsp", "lspServers": {}},
                {"name": "hooks", "hooks": {"UserPromptSubmit": []}},
            ]),
        );
        assert_eq!(hooked(&home.config()).unwrap(), ["hooks@shop"]);
    }

    #[test]
    fn no_plugin_list_means_no_plugins_and_a_broken_one_refuses() {
        let home = Home::new();
        assert!(hooked(&home.config()).unwrap().is_empty());
        fs::write(home.dir.path().join(INSTALLED), "not json").unwrap();
        let error = hooked(&home.config()).unwrap_err();
        assert!(error.contains("is not JSON"), "{error}");
    }

    #[test]
    fn a_config_path_that_is_a_file_means_no_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("claude");
        fs::write(&file, "#!/bin/sh\n").unwrap();
        assert!(hooked(&ClaudeConfig::at(&file)).unwrap().is_empty());
    }

    #[test]
    fn switch_off_names_each_plugin_false_and_keeps_the_rest() {
        let mut fragment = json!({"permissions": {}, "enabledPlugins": {"kept@shop": true}});
        switch_off(&mut fragment, &["notes@shop".to_owned()]);
        assert_eq!(
            fragment["enabledPlugins"],
            json!({"kept@shop": true, "notes@shop": false})
        );
        let mut empty = json!({"permissions": {}});
        switch_off(&mut empty, &[]);
        assert_eq!(empty, json!({"permissions": {}}));
    }
}
