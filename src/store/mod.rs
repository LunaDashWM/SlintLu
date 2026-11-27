//! A small persistent key–value store for shell settings (wallpaper, toggles, positions …),
//! saved as JSON.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Default)]
pub struct Store {
    path: Option<PathBuf>,
    values: BTreeMap<String, Value>,
}

impl Store {
    /// Loads `path` (an empty store when it does not exist or is broken).
    pub fn load(path: &Path) -> Self {
        let values = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path: Some(path.to_path_buf()), values }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// Sets a value and writes the file (atomically, through a temporary file).
    pub fn set(&mut self, key: &str, value: Value) {
        if self.values.get(key) == Some(&value) {
            return;
        }
        self.values.insert(key.to_owned(), value);
        self.save();
    }

    pub fn remove(&mut self, key: &str) {
        if self.values.remove(key).is_some() {
            self.save();
        }
    }

    /// Keys in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.values.keys()
    }

    /// Every value whose key starts with one of `prefixes`.
    pub fn matching(&self, prefixes: &[String]) -> BTreeMap<String, Value> {
        self.values
            .iter()
            .filter(|(key, _)| matches(key, prefixes))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// Replaces every value whose key starts with one of `prefixes` by those of `values` that
    /// do, and writes the file once.
    pub fn replace_matching(&mut self, prefixes: &[String], values: BTreeMap<String, Value>) {
        let mut next: BTreeMap<String, Value> = std::mem::take(&mut self.values)
            .into_iter()
            .filter(|(k, _)| !matches(k, prefixes))
            .collect();
        next.extend(values.into_iter().filter(|(k, _)| matches(k, prefixes)));
        self.values = next;
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let temporary = path.with_extension("json.tmp");
            std::fs::write(
                &temporary,
                serde_json::to_vec_pretty(&self.values).map_err(std::io::Error::other)?,
            )?;
            std::fs::rename(temporary, path)
        })();
        if let Err(e) = result {
            log::warn!("cannot save {}: {e}", path.display());
        }
    }
}

fn matches(key: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| key.starts_with(p.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_keys_by_prefix() {
        let mut store = Store::default();
        store.set("widget-x:a", json!(1));
        store.set("widgets", json!(["a"]));
        store.set("dock", json!(true));
        let prefixes = vec!["widget".to_owned()];
        assert_eq!(store.matching(&prefixes).len(), 2);
        let next = BTreeMap::from([
            ("widget-x:b".to_owned(), json!(2)),
            ("dock".to_owned(), json!(false)),
        ]);
        store.replace_matching(&prefixes, next);
        assert_eq!(store.keys().collect::<Vec<_>>(), ["dock", "widget-x:b"]);
        assert_eq!(store.get("dock"), Some(&json!(true)));
    }

    #[test]
    fn values_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/store.json");
        let mut store = Store::load(&path);
        store.set("wallpaper", json!("/w.png"));
        store.set("dock", json!(true));
        store.set("scale", json!(1.5));
        let reloaded = Store::load(&path);
        assert_eq!(reloaded.get("wallpaper"), Some(&json!("/w.png")));
        assert_eq!(reloaded.get("dock"), Some(&json!(true)));
        let mut reloaded = reloaded;
        reloaded.remove("dock");
        assert_eq!(Store::load(&path).get("dock"), None);
    }

    #[test]
    fn broken_files_start_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Store::load(&path).get("x"), None);
    }
}
