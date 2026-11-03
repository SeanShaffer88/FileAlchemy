//! `data/config.json` — the complete persisted state (spec §5).
//!
//! Rules:
//! * missing or corrupt file -> built-in defaults, never a crash;
//! * unknown keys are ignored (forward compatibility);
//! * writes are atomic (temp file + rename) so a power cut cannot leave a
//!   half-written config that bricks the next launch.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::engine::formats::{CollisionPolicy, OutputStrategy};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// 1..=16. Higher = more files at once.
    pub max_concurrent_tasks: usize,
    pub output_strategy: OutputStrategy,
    /// Empty when `output_strategy` is `SameAsSource`.
    pub custom_output_dir: String,
    pub collision_policy: CollisionPolicy,
    pub shell_integration: ShellIntegration,
    pub notifications: Notifications,
    pub cleanup: Cleanup,
    /// Remembered window geometry (spec §2 mentions "UI geometry").
    pub window: WindowGeometry,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ShellIntegration {
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Notifications {
    pub play_sound_on_complete: bool,
    pub flash_taskbar_on_complete: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            play_sound_on_complete: true,
            flash_taskbar_on_complete: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Cleanup {
    pub purge_temp_on_cancel: bool,
}

impl Default for Cleanup {
    fn default() -> Self {
        Cleanup {
            purge_temp_on_cancel: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct WindowGeometry {
    pub width: i32,
    pub height: i32,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        WindowGeometry {
            width: 820,
            height: 600,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            max_concurrent_tasks: 4,
            output_strategy: OutputStrategy::SameAsSource,
            custom_output_dir: String::new(),
            collision_policy: CollisionPolicy::AutoRename,
            shell_integration: ShellIntegration::default(),
            notifications: Notifications::default(),
            cleanup: Cleanup::default(),
            window: WindowGeometry::default(),
        }
    }
}

impl Config {
    /// Load, or fall back to defaults. Never fails: a broken config must not
    /// stop the user from converting files.
    pub fn load_or_default(path: &Path) -> (Config, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Config::default(), None)
            }
            Err(e) => {
                return (
                    Config::default(),
                    Some(format!("config.json could not be read ({e}); using defaults")),
                )
            }
        };

        match serde_json::from_str::<Config>(&text) {
            Ok(config) => (config.sanitised(), None),
            Err(e) => (
                Config::default(),
                Some(format!(
                    "config.json was not valid JSON ({e}); using defaults"
                )),
            ),
        }
    }

    /// Keep values inside the range the UI and engine can honour.
    pub fn sanitised(mut self) -> Config {
        self.version = CONFIG_VERSION;
        self.max_concurrent_tasks = self.max_concurrent_tasks.clamp(1, 16);
        self.window.width = self.window.width.clamp(720, 3840);
        self.window.height = self.window.height.clamp(480, 2160);
        if self.output_strategy == OutputStrategy::CustomDir && self.custom_output_dir.trim().is_empty()
        {
            // A custom strategy with no folder would write nowhere useful.
            self.output_strategy = OutputStrategy::SameAsSource;
        }
        self
    }

    /// Atomic save: write `config.json.tmp`, flush, then rename over the real
    /// file. `data/` is created if it does not exist yet.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let temp = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(json.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        match std::fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&temp);
                Err(e)
            }
        }
    }

    pub fn concurrency_index(&self) -> usize {
        const CHOICES: [usize; 4] = [1, 2, 4, 8];
        CHOICES
            .iter()
            .position(|n| *n >= self.max_concurrent_tasks)
            .unwrap_or(CHOICES.len() - 1)
    }

    pub fn concurrency_from_index(index: usize) -> usize {
        const CHOICES: [usize; 4] = [1, 2, 4, 8];
        *CHOICES.get(index).unwrap_or(&4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::from_wide;

    fn temp_config_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "filealchemy-config-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    #[test]
    fn defaults_match_the_documented_schema() {
        let config = Config::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("\"version\": 1"), "{json}");
        assert!(json.contains("\"max_concurrent_tasks\": 4"), "{json}");
        assert!(json.contains("\"output_strategy\": \"SameAsSource\""), "{json}");
        assert!(json.contains("\"custom_output_dir\": \"\""), "{json}");
        assert!(json.contains("\"collision_policy\": \"AutoRename\""), "{json}");
        assert!(json.contains("\"play_sound_on_complete\": true"), "{json}");
        assert!(json.contains("\"flash_taskbar_on_complete\": true"), "{json}");
        assert!(json.contains("\"purge_temp_on_cancel\": true"), "{json}");
        assert!(json.contains("\"enabled\": false"), "{json}");
    }

    #[test]
    fn round_trip_through_disk() {
        let path = temp_config_path("roundtrip");
        let mut config = Config::default();
        config.max_concurrent_tasks = 2;
        config.collision_policy = CollisionPolicy::Overwrite;
        config.custom_output_dir = r"C:\converted".into();
        config.output_strategy = OutputStrategy::CustomDir;
        config.shell_integration.enabled = true;
        config.save(&path).unwrap();

        let (loaded, warning) = Config::load_or_default(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, config);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        assert!(!text.contains("json.tmp"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_file_uses_defaults_silently() {
        let path = temp_config_path("missing");
        let (config, warning) = Config::load_or_default(&path);
        assert_eq!(config, Config::default());
        assert!(warning.is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn corrupt_file_uses_defaults_with_a_warning() {
        let path = temp_config_path("corrupt");
        std::fs::write(&path, b"{ this is not json").unwrap();
        let (config, warning) = Config::load_or_default(&path);
        assert_eq!(config, Config::default());
        assert!(warning.unwrap().contains("not valid JSON"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn future_versions_with_unknown_keys_still_load() {
        let path = temp_config_path("future");
        std::fs::write(
            &path,
            br#"{"version": 9, "max_concurrent_tasks": 3, "brand_new_option": true}"#,
        )
        .unwrap();
        let (config, warning) = Config::load_or_default(&path);
        assert!(warning.is_none());
        assert_eq!(config.max_concurrent_tasks, 3);
        assert_eq!(config.version, CONFIG_VERSION);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let path = temp_config_path("clamp");
        std::fs::write(
            &path,
            br#"{"max_concurrent_tasks": 999, "window": {"width": 10, "height": 9999}}"#,
        )
        .unwrap();
        let (config, _) = Config::load_or_default(&path);
        assert_eq!(config.max_concurrent_tasks, 16);
        assert_eq!(config.window.width, 720);
        assert_eq!(config.window.height, 2160);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn custom_strategy_without_a_folder_falls_back() {
        let path = temp_config_path("nostrategy");
        std::fs::write(
            &path,
            br#"{"output_strategy": "CustomDir", "custom_output_dir": "   "}"#,
        )
        .unwrap();
        let (config, _) = Config::load_or_default(&path);
        assert_eq!(config.output_strategy, OutputStrategy::SameAsSource);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn concurrency_index_maps_to_the_ui_choices() {
        let mut config = Config::default();
        for (index, expected) in [(0usize, 1usize), (1, 2), (2, 4), (3, 8)] {
            config.max_concurrent_tasks = Config::concurrency_from_index(index);
            assert_eq!(config.max_concurrent_tasks, expected);
            assert_eq!(config.concurrency_index(), index);
        }
        // The platform helper is exercised here so the module stays honest.
        assert_eq!(from_wide(&[b'x' as u16, 0, b'y' as u16]), "x");
    }
}
