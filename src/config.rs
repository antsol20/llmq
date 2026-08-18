//! Config loading. Defaults live in the `Default` impls, and `#[serde(default)]`
//! does the merging that `deep_merge` used to do by hand.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug)]
pub enum ConfigError {
    Missing(PathBuf),
    Parse(PathBuf, String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Missing(p) => write!(
                f,
                "no config at {}\n  mkdir -p {}\n  cp config.example.toml {}   # then set url + key",
                p.display(),
                p.parent().unwrap_or(Path::new("")).display(),
                p.display()
            ),
            ConfigError::Parse(p, msg) => write!(f, "{}: {}", p.display(), msg),
        }
    }
}

/// TOML distinguishes `1` from `1.0`; accept either so `temperature = 1` works.
fn lenient_f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Num {
        F(f64),
        I(i64),
    }
    Ok(match Num::deserialize(d)? {
        Num::F(v) => v,
        Num::I(v) => v as f64,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Api {
    pub url: String,
    pub key: String,
    pub key_command: String,
    pub model: String,
    #[serde(deserialize_with = "lenient_f64")]
    pub temperature: f64,
    pub max_tokens: u32,
    pub timeout: u64,
    /// Merged verbatim into the request body. Every provider spells "don't
    /// think" differently — `reasoning`, `reasoning_effort`, `thinkingConfig` —
    /// so llmq stays ignorant and forwards whatever the config names.
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Api {
    fn default() -> Self {
        Self {
            url: "https://api.openai.com/v1".into(),
            key: String::new(),
            key_command: String::new(),
            model: "gpt-4o-mini".into(),
            temperature: 0.2,
            max_tokens: 1024,
            timeout: 60,
            extra: serde_json::Map::new(),
        }
    }
}

/// Body keys llmq writes itself. An `[api.extra]` entry that shadowed one of
/// these would be silently ignored at request time, so reject it at load.
const RESERVED: [(&str, &str); 5] = [
    ("model", "model"),
    ("messages", "prompt.system / the question"),
    ("stream", "always true; llmq only speaks SSE"),
    ("temperature", "temperature"),
    ("max_tokens", "max_tokens"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prompt {
    pub system: String,
    pub include_context: bool,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            system: "You are a terminal assistant. Reply with shell commands only. \
                     No prose, no explanation, no markdown fences. \
                     One command per line. If several approaches exist, give each on its own line."
                .into(),
            include_context: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCfg {
    pub width_pct: u16,
    pub height_pct: u16,
    pub title: String,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            width_pct: 80,
            height_pct: 60,
            title: " llmq ".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TmuxCfg {
    pub enabled: bool,
    pub width: String,
    pub height: String,
}

impl Default for TmuxCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            width: "80%".into(),
            height: "60%".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub api: Api,
    pub prompt: Prompt,
    pub ui: UiCfg,
    pub tmux: TmuxCfg,
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("llmq").join("config.toml")
}

pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
    let owned = path.map(PathBuf::from).unwrap_or_else(default_path);
    if !owned.exists() {
        return Err(ConfigError::Missing(owned));
    }
    let text = std::fs::read_to_string(&owned)
        .map_err(|e| ConfigError::Parse(owned.clone(), e.to_string()))?;
    let cfg: Config = toml::from_str(&text)
        .map_err(|e| ConfigError::Parse(owned.clone(), e.message().to_string()))?;
    if let Err(msg) = check_extra(&cfg.api) {
        return Err(ConfigError::Parse(owned, msg));
    }
    Ok(cfg)
}

fn check_extra(api: &Api) -> Result<(), String> {
    for (key, owner) in RESERVED {
        if api.extra.contains_key(key) {
            return Err(format!(
                "[api.extra] may not set `{key}` — llmq writes it from {owner}"
            ));
        }
    }
    Ok(())
}

/// Key can be literal, `$ENV_VAR`, or the stdout of `key_command`.
pub fn resolve_key(api: &Api) -> Result<String, String> {
    if !api.key_command.is_empty() {
        let parts = shlex::split(&api.key_command)
            .ok_or_else(|| format!("key_command is not valid shell: {}", api.key_command))?;
        let (exe, rest) = parts
            .split_first()
            .ok_or_else(|| "key_command is empty".to_string())?;
        let out = Command::new(exe)
            .args(rest)
            .output()
            .map_err(|e| format!("{exe}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{exe} exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let key = api.key.trim();
    if let Some(var) = key.strip_prefix('$') {
        return Ok(std::env::var(var).unwrap_or_default());
    }
    Ok(key.to_string())
}

pub fn endpoint_url(url: &str) -> String {
    let u = url.trim_end_matches('/');
    if u.ends_with("/chat/completions") {
        u.to_string()
    } else {
        format!("{u}/chat/completions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tables_fall_back_to_defaults() {
        let cfg: Config = toml::from_str("[api]\nmodel = \"m\"\n").unwrap();
        assert_eq!(cfg.api.model, "m");
        assert_eq!(cfg.api.url, Api::default().url); // untouched key in same table
        assert_eq!(cfg.ui.width_pct, 80); // whole table absent
        assert!(cfg.tmux.enabled);
    }

    #[test]
    fn extra_survives_toml_to_json() {
        let cfg: Config = toml::from_str(
            "[api.extra]\nreasoning = { enabled = false }\nprovider = { sort = \"latency\" }\ntop_p = 0.9\n",
        )
        .unwrap();
        let extra = &cfg.api.extra;
        assert_eq!(extra["reasoning"]["enabled"], serde_json::json!(false));
        assert_eq!(extra["provider"]["sort"], serde_json::json!("latency"));
        assert_eq!(extra["top_p"], serde_json::json!(0.9));
        assert_eq!(cfg.api.model, Api::default().model); // sibling keys untouched
    }

    #[test]
    fn extra_defaults_to_empty() {
        let cfg: Config = toml::from_str("[api]\nmodel = \"m\"\n").unwrap();
        assert!(cfg.api.extra.is_empty());
        assert!(check_extra(&cfg.api).is_ok());
    }

    #[test]
    fn extra_may_not_shadow_a_key_llmq_owns() {
        for key in ["model", "messages", "stream", "temperature", "max_tokens"] {
            let cfg: Config = toml::from_str(&format!("[api.extra]\n{key} = \"x\"\n")).unwrap();
            let err = check_extra(&cfg.api).unwrap_err();
            assert!(err.contains(key), "{key}: {err}");
        }
    }

    #[test]
    fn temperature_accepts_int_or_float() {
        assert_eq!(
            toml::from_str::<Config>("[api]\ntemperature = 1\n")
                .unwrap()
                .api
                .temperature,
            1.0
        );
        assert_eq!(
            toml::from_str::<Config>("[api]\ntemperature = 0.7\n")
                .unwrap()
                .api
                .temperature,
            0.7
        );
    }

    #[test]
    fn endpoint_is_appended_only_once() {
        assert_eq!(
            endpoint_url("https://x/v1"),
            "https://x/v1/chat/completions"
        );
        assert_eq!(
            endpoint_url("https://x/v1/"),
            "https://x/v1/chat/completions"
        );
        assert_eq!(
            endpoint_url("https://x/v1/chat/completions"),
            "https://x/v1/chat/completions"
        );
    }

    #[test]
    fn key_forms() {
        let literal = Api {
            key: "  sk-abc  ".into(),
            ..Default::default()
        };
        assert_eq!(resolve_key(&literal).unwrap(), "sk-abc");

        std::env::set_var("LLMQ_TEST_KEY", "from-env");
        let env = Api {
            key: "$LLMQ_TEST_KEY".into(),
            ..Default::default()
        };
        assert_eq!(resolve_key(&env).unwrap(), "from-env");

        let cmd = Api {
            key_command: "printf sk-from-cmd".into(),
            ..Default::default()
        };
        assert_eq!(resolve_key(&cmd).unwrap(), "sk-from-cmd");
    }

    #[test]
    fn failing_key_command_is_an_error_not_an_empty_key() {
        let cmd = Api {
            key_command: "false".into(),
            ..Default::default()
        };
        assert!(resolve_key(&cmd).is_err());
    }
}
