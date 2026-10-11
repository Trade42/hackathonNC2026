//! Typed configuration from the one root `.env` and the process environment.
//!
//! The file is `FERRY_ENV_FILE` when set, otherwise the `.env` in the first directory, walking up
//! from the current directory, that holds both `.env` and `contract/` (the repo root). The process
//! environment always wins over the file, so a worktree can override ports and URLs. An empty
//! value means unset. Secret values are [`Secret`]s, which never print.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::auth::demo::DemoPersona;

/// Names ferry-core reads, in `.env.example` order. Every one is documented in `.env.example`.
pub const NAMES: [&str; 33] = [
    "DATABASE_URL",
    "TEST_DATABASE_URL",
    "SUPABASE_URL",
    "SUPABASE_PUBLISHABLE_KEY",
    "SUPABASE_SECRET_KEY",
    "JWT_SECRET",
    "NESSIE_API_KEY",
    "NESSIE_BASE_URL",
    "NESSIE_MODE",
    "GEMINI_API_KEY",
    "GEMINI_MODEL",
    "PHOTON_PROJECT_ID",
    "PHOTON_PROJECT_SECRET",
    "SIDECAR_URL",
    "SIDECAR_SHARED_SECRET",
    "PORT",
    "CORE_URL",
    "AGENT_PHONE_E164",
    "AGENT_PHONE_DISPLAY",
    "PUBLIC_WEB_URL",
    "TELEGRAM_BOT_USERNAME",
    "VAPID_PUBLIC_KEY",
    "VAPID_PRIVATE_KEY",
    "DEMO_MODE",
    "DEMO_TELEGRAM_PERSONAS",
    "DEMO_SIM_SPEED",
    "TAX_RATE_BPS",
    "FERRY_FEE_CENTS",
    "TIGER_DATABASE_URL",
    "OSRM_BASE_URL",
    "GEMINI_BASE_URL",
    "PUSH_TEST_STUB_ORIGIN",
    // Not in .env.example: it chooses the file itself, so it only comes from the process.
    "FERRY_ENV_FILE",
];

/// `DEMO_SIM_SPEED` when unset: at 8× a brisk 1.6 m/s, a 1 km walk takes about 78 s.
pub const DEFAULT_DEMO_SIM_SPEED: f64 = 8.0;

/// The `DEMO_SIM_SPEED` values used; anything else is the default, with a warning. Below 0.5 the
/// runner barely leaves the restaurant; above 100 the walk is a ping or two.
pub const DEMO_SIM_SPEED_RANGE: std::ops::RangeInclusive<f64> = 0.5..=100.0;

/// The variable that names the env file explicitly.
pub const ENV_FILE_VAR: &str = "FERRY_ENV_FILE";

/// A secret value. `Debug` prints `Secret(..)`; read it with [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NessieMode {
    Live,
    Mock,
}

/// What became of `DEMO_TELEGRAM_PERSONAS`, for [`Config::summary`], which never names its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoPersonasSetting {
    Unset,
    /// Read in demo mode and valid. A value with no pairs (`,`) is set and lists no personas.
    Set,
    /// Set while `DEMO_MODE` is off, so not parsed.
    Ignored,
    /// Set in demo mode but malformed, so no personas.
    Invalid,
}

impl DemoPersonasSetting {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Set => "set",
            Self::Ignored => "ignored",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Supabase session pooler (port 5432); a local database in tests and worktree runs.
    pub database_url: Secret,
    pub test_database_url: Option<Secret>,
    /// Without a trailing slash.
    pub supabase_url: Option<String>,
    pub supabase_publishable_key: Option<String>,
    pub supabase_secret_key: Option<Secret>,
    pub jwt_secret: Option<Secret>,
    pub nessie_api_key: Option<Secret>,
    pub nessie_base_url: Option<String>,
    /// Unset means mock.
    pub nessie_mode: NessieMode,
    pub gemini_api_key: Option<Secret>,
    pub gemini_model: Option<String>,
    pub photon_project_id: Option<String>,
    pub photon_project_secret: Option<Secret>,
    pub sidecar_url: Option<String>,
    pub sidecar_shared_secret: Option<Secret>,
    /// Default 8080.
    pub port: u16,
    /// Where the sidecar reaches core. Default `http://127.0.0.1:8080`.
    pub core_url: String,
    pub agent_phone_e164: Option<String>,
    pub agent_phone_display: Option<String>,
    pub public_web_url: Option<String>,
    /// The Telegram bot's username, without the leading `@` (for `t.me` links).
    pub telegram_bot_username: Option<String>,
    pub vapid_public_key: Option<String>,
    pub vapid_private_key: Option<Secret>,
    /// Default false.
    pub demo_mode: bool,
    /// `DEMO_TELEGRAM_PERSONAS`: which Telegram usernames sign in as which demo persona. Read only
    /// when `demo_mode` is true, and never fatal ([`demo_telegram_personas`]). Usernames are without
    /// the `@`. Default empty.
    pub demo_telegram_personas: Vec<(DemoPersona, String)>,
    /// What became of `DEMO_TELEGRAM_PERSONAS`: set, ignored or invalid. Only the summary reads it.
    pub demo_telegram_personas_setting: DemoPersonasSetting,
    /// The demo runner simulator's walking-speed multiplier over the runner's pace. Read only when
    /// `demo_mode` is true, and never fatal (`demo_sim_speed()`): in [`DEMO_SIM_SPEED_RANGE`],
    /// otherwise [`DEFAULT_DEMO_SIM_SPEED`].
    pub demo_sim_speed: f64,
    /// Default 750.
    pub tax_rate_bps: i64,
    /// Default 150.
    pub ferry_fee_cents: i64,
    pub tiger_database_url: Option<Secret>,
    /// OSRM routing server, without a trailing slash. Unset means the FOSSGIS server
    /// (`tracking::OSRM_DEFAULT_BASE_URL`).
    pub osrm_base_url: Option<String>,
    /// Gemini's REST API, without a trailing slash. Unset means Google's
    /// (`agent::gemini::DEFAULT_BASE_URL`); tests point it at a local stub.
    pub gemini_base_url: Option<String>,
    /// Tests and local runs only: a local stub push service's origin that Web Push may send to.
    /// Honored only with a local DATABASE_URL (`notifications::push::PushSettings::from_config`);
    /// never fatal here, since a bad value is only ignored there.
    pub push_test_stub_origin: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
    #[error("{ENV_FILE_VAR} names {0}, which is not a readable file")]
    EnvFileMissing(PathBuf),
    // The line itself is left out: it may hold a secret.
    #[error("cannot parse {path} near line {line}")]
    EnvFileParse { path: PathBuf, line: usize },
    #[error("cannot read {path}: {source}")]
    EnvFileRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot read the current directory: {0}")]
    CurrentDir(std::io::Error),
}

impl Config {
    /// Loads the env file and the process environment. Returns the file used, if any.
    pub fn load() -> Result<(Self, Option<PathBuf>), ConfigError> {
        let process: HashMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let cwd = std::env::current_dir().map_err(ConfigError::CurrentDir)?;
        let env_file = find_env_file(process.get(ENV_FILE_VAR).map(String::as_str), &cwd)?;
        let mut vars = match &env_file {
            Some(path) => read_env_file(path)?,
            None => HashMap::new(),
        };
        // The process environment wins over the file.
        vars.extend(process);
        Ok((Self::from_vars(&vars)?, env_file))
    }

    /// Builds the config from a map of variables. Tests use this directly.
    pub fn from_vars(vars: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let get = |name: &'static str| -> Option<String> {
            vars.get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let secret = |name: &'static str| get(name).map(Secret::new);
        let url = |name: &'static str| -> Result<Option<String>, ConfigError> {
            get(name).map(|v| parse_url(name, &v)).transpose()
        };
        let demo_mode = match get("DEMO_MODE") {
            None => false,
            Some(v) => {
                parse_bool(&v).ok_or_else(|| invalid("DEMO_MODE", "expected true or false"))?
            }
        };
        let (demo_telegram_personas, demo_telegram_personas_setting) =
            demo_telegram_personas(demo_mode, get("DEMO_TELEGRAM_PERSONAS").as_deref());

        Ok(Self {
            database_url: secret("DATABASE_URL").ok_or(ConfigError::Missing("DATABASE_URL"))?,
            test_database_url: secret("TEST_DATABASE_URL"),
            supabase_url: url("SUPABASE_URL")?,
            supabase_publishable_key: get("SUPABASE_PUBLISHABLE_KEY"),
            supabase_secret_key: secret("SUPABASE_SECRET_KEY"),
            jwt_secret: secret("JWT_SECRET"),
            nessie_api_key: secret("NESSIE_API_KEY"),
            nessie_base_url: url("NESSIE_BASE_URL")?,
            nessie_mode: match get("NESSIE_MODE").as_deref() {
                None | Some("mock") => NessieMode::Mock,
                Some("live") => NessieMode::Live,
                Some(_) => return Err(invalid("NESSIE_MODE", "expected live or mock")),
            },
            gemini_api_key: secret("GEMINI_API_KEY"),
            gemini_model: get("GEMINI_MODEL"),
            photon_project_id: get("PHOTON_PROJECT_ID"),
            photon_project_secret: secret("PHOTON_PROJECT_SECRET"),
            sidecar_url: url("SIDECAR_URL")?,
            sidecar_shared_secret: secret("SIDECAR_SHARED_SECRET"),
            port: match get("PORT") {
                None => 8080,
                Some(v) => v
                    .parse()
                    .map_err(|_| invalid("PORT", "expected a port number"))?,
            },
            core_url: url("CORE_URL")?.unwrap_or_else(|| "http://127.0.0.1:8080".to_owned()),
            agent_phone_e164: get("AGENT_PHONE_E164"),
            agent_phone_display: get("AGENT_PHONE_DISPLAY"),
            public_web_url: url("PUBLIC_WEB_URL")?,
            telegram_bot_username: get("TELEGRAM_BOT_USERNAME")
                .map(|v| v.trim_start_matches('@').to_owned())
                .filter(|v| !v.is_empty()),
            vapid_public_key: get("VAPID_PUBLIC_KEY"),
            vapid_private_key: secret("VAPID_PRIVATE_KEY"),
            demo_mode,
            demo_telegram_personas,
            demo_telegram_personas_setting,
            demo_sim_speed: match get("DEMO_SIM_SPEED") {
                None => DEFAULT_DEMO_SIM_SPEED,
                Some(v) => demo_sim_speed(demo_mode, &v),
            },
            tax_rate_bps: int_or("TAX_RATE_BPS", get("TAX_RATE_BPS"), 750)?,
            ferry_fee_cents: int_or("FERRY_FEE_CENTS", get("FERRY_FEE_CENTS"), 150)?,
            tiger_database_url: secret("TIGER_DATABASE_URL"),
            osrm_base_url: url("OSRM_BASE_URL")?,
            gemini_base_url: url("GEMINI_BASE_URL")?,
            push_test_stub_origin: get("PUSH_TEST_STUB_ORIGIN"),
        })
    }

    /// The demo persona whose Telegram username `DEMO_TELEGRAM_PERSONAS` lists as `username`
    /// (compared without case, as Telegram does), only when `demo_mode` is true.
    pub fn demo_persona_for_telegram(&self, username: &str) -> Option<DemoPersona> {
        if !self.demo_mode {
            return None;
        }
        let username = username.trim_start_matches('@');
        self.demo_telegram_personas
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(username))
            .map(|(persona, _)| *persona)
    }

    /// One line naming each variable as set or unset; values only where they are not secret.
    pub fn summary(&self) -> String {
        let set = |is_set: bool| if is_set { "set" } else { "unset" };
        let pairs: [(&str, &str); 27] = [
            ("TEST_DATABASE_URL", set(self.test_database_url.is_some())),
            ("SUPABASE_URL", set(self.supabase_url.is_some())),
            (
                "SUPABASE_PUBLISHABLE_KEY",
                set(self.supabase_publishable_key.is_some()),
            ),
            (
                "SUPABASE_SECRET_KEY",
                set(self.supabase_secret_key.is_some()),
            ),
            ("JWT_SECRET", set(self.jwt_secret.is_some())),
            ("NESSIE_API_KEY", set(self.nessie_api_key.is_some())),
            ("NESSIE_BASE_URL", set(self.nessie_base_url.is_some())),
            ("GEMINI_API_KEY", set(self.gemini_api_key.is_some())),
            ("GEMINI_MODEL", set(self.gemini_model.is_some())),
            ("PHOTON_PROJECT_ID", set(self.photon_project_id.is_some())),
            (
                "PHOTON_PROJECT_SECRET",
                set(self.photon_project_secret.is_some()),
            ),
            ("SIDECAR_URL", set(self.sidecar_url.is_some())),
            (
                "SIDECAR_SHARED_SECRET",
                set(self.sidecar_shared_secret.is_some()),
            ),
            ("AGENT_PHONE_E164", set(self.agent_phone_e164.is_some())),
            (
                "AGENT_PHONE_DISPLAY",
                set(self.agent_phone_display.is_some()),
            ),
            ("PUBLIC_WEB_URL", set(self.public_web_url.is_some())),
            (
                "TELEGRAM_BOT_USERNAME",
                set(self.telegram_bot_username.is_some()),
            ),
            ("VAPID_PUBLIC_KEY", set(self.vapid_public_key.is_some())),
            ("VAPID_PRIVATE_KEY", set(self.vapid_private_key.is_some())),
            ("TIGER_DATABASE_URL", set(self.tiger_database_url.is_some())),
            ("OSRM_BASE_URL", set(self.osrm_base_url.is_some())),
            ("GEMINI_BASE_URL", set(self.gemini_base_url.is_some())),
            (
                "PUSH_TEST_STUB_ORIGIN",
                set(self.push_test_stub_origin.is_some()),
            ),
            (
                "NESSIE_MODE",
                if self.nessie_mode == NessieMode::Live {
                    "live"
                } else {
                    "mock"
                },
            ),
            ("DEMO_MODE", if self.demo_mode { "true" } else { "false" }),
            // Usernames are masked in logs, so the summary names none of them.
            (
                "DEMO_TELEGRAM_PERSONAS",
                self.demo_telegram_personas_setting.as_str(),
            ),
            ("DATABASE_URL", "set"),
        ];
        let mut line = format!(
            "PORT={} TAX_RATE_BPS={} FERRY_FEE_CENTS={} DEMO_SIM_SPEED={}",
            self.port, self.tax_rate_bps, self.ferry_fee_cents, self.demo_sim_speed
        );
        for (name, value) in pairs {
            line.push_str(&format!(" {name}={value}"));
        }
        line
    }
}

/// The env file to load: `explicit` (from `FERRY_ENV_FILE`) when given, otherwise the `.env` in
/// the first directory at or above `start` that also holds `contract/`.
pub fn find_env_file(explicit: Option<&str>, start: &Path) -> Result<Option<PathBuf>, ConfigError> {
    if let Some(path) = explicit.map(str::trim).filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(Some(path))
        } else {
            Err(ConfigError::EnvFileMissing(path))
        };
    }
    Ok(start
        .ancestors()
        .find(|dir| dir.join(".env").is_file() && dir.join("contract").is_dir())
        .map(|dir| dir.join(".env")))
}

/// Parses an env file into a map, without touching the process environment.
pub fn read_env_file(path: &Path) -> Result<HashMap<String, String>, ConfigError> {
    let iter = dotenvy::from_path_iter(path).map_err(|e| match e {
        dotenvy::Error::Io(source) => ConfigError::EnvFileRead {
            path: path.to_owned(),
            source,
        },
        _ => ConfigError::EnvFileParse {
            path: path.to_owned(),
            line: 0,
        },
    })?;
    let mut vars = HashMap::new();
    for item in iter {
        match item {
            Ok((key, value)) => {
                vars.insert(key, value);
            }
            Err(dotenvy::Error::LineParse(_, index)) => {
                return Err(ConfigError::EnvFileParse {
                    path: path.to_owned(),
                    line: index + 1,
                });
            }
            Err(dotenvy::Error::Io(source)) => {
                return Err(ConfigError::EnvFileRead {
                    path: path.to_owned(),
                    source,
                });
            }
            Err(_) => {
                return Err(ConfigError::EnvFileParse {
                    path: path.to_owned(),
                    line: vars.len() + 1,
                });
            }
        }
    }
    Ok(vars)
}

fn invalid(name: &'static str, reason: &str) -> ConfigError {
    ConfigError::Invalid {
        name,
        reason: reason.to_owned(),
    }
}

fn parse_url(name: &'static str, value: &str) -> Result<String, ConfigError> {
    match url::Url::parse(value) {
        Ok(u) if u.has_host() => Ok(value.trim_end_matches('/').to_owned()),
        _ => Err(invalid(
            name,
            "expected an absolute URL such as https://example.com",
        )),
    }
}

/// `DEMO_TELEGRAM_PERSONAS` (`value`), which never stops the server (PR #17 review MINOR 3). With
/// demo mode off it is not parsed: a set value is ignored with one warning. With demo mode on, a
/// malformed value is one error line and no personas. Neither line quotes the value. Also returns
/// what became of it, for the summary.
fn demo_telegram_personas(
    demo_mode: bool,
    value: Option<&str>,
) -> (Vec<(DemoPersona, String)>, DemoPersonasSetting) {
    let Some(value) = value else {
        return (Vec::new(), DemoPersonasSetting::Unset);
    };
    if !demo_mode {
        tracing::warn!("DEMO_TELEGRAM_PERSONAS is set but DEMO_MODE is off, so it is ignored");
        return (Vec::new(), DemoPersonasSetting::Ignored);
    }
    match parse_demo_telegram_personas(value) {
        Ok(pairs) => (pairs, DemoPersonasSetting::Set),
        Err(reason) => {
            tracing::error!(
                "DEMO_TELEGRAM_PERSONAS is invalid ({reason}); demo personas are disabled"
            );
            (Vec::new(), DemoPersonasSetting::Invalid)
        }
    }
}

/// `persona=@username` pairs, comma separated: `maya=@maya_demo,judge=@someone`. The `@` is
/// optional. Each persona and each username (without case) appears at most once. The errors never
/// quote the value, as the other config errors don't.
fn parse_demo_telegram_personas(value: &str) -> Result<Vec<(DemoPersona, String)>, &'static str> {
    let mut pairs: Vec<(DemoPersona, String)> = Vec::new();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (persona, username) = entry
            .split_once('=')
            .ok_or("expected persona=@username pairs separated by commas")?;
        let persona = DemoPersona::from_name(persona.trim())
            .ok_or("a persona is not one of judge, maya, dana and leo")?;
        let username = username.trim().trim_start_matches('@');
        if username.is_empty()
            || username.len() > 64
            || !username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err("a username is not a Telegram username (letters, digits and _)");
        }
        if pairs
            .iter()
            .any(|(p, u)| *p == persona || u.eq_ignore_ascii_case(username))
        {
            return Err("a persona or a username appears twice");
        }
        pairs.push((persona, username.to_owned()));
    }
    Ok(pairs)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn int_or(name: &'static str, value: Option<String>, default: i64) -> Result<i64, ConfigError> {
    match value {
        None => Ok(default),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| invalid(name, "expected a whole number of at least 0")),
    }
}

/// `DEMO_SIM_SPEED` (`value`, which is set), which never stops the server (runner-sim review
/// MINOR 2). With demo mode off it is not parsed: it is ignored with one warning. With demo mode
/// on, a value that is not a number in [`DEMO_SIM_SPEED_RANGE`] is one warning and the default.
/// Neither line quotes the value, as the other config errors don't.
fn demo_sim_speed(demo_mode: bool, value: &str) -> f64 {
    if !demo_mode {
        tracing::warn!("DEMO_SIM_SPEED is set but DEMO_MODE is off, so it is ignored");
        return DEFAULT_DEMO_SIM_SPEED;
    }
    match value.parse::<f64>() {
        Ok(speed) if DEMO_SIM_SPEED_RANGE.contains(&speed) => speed,
        _ => {
            tracing::warn!(
                "DEMO_SIM_SPEED is not a number from {} to {}, so the default {DEFAULT_DEMO_SIM_SPEED} is used",
                DEMO_SIM_SPEED_RANGE.start(),
                DEMO_SIM_SPEED_RANGE.end()
            );
            DEFAULT_DEMO_SIM_SPEED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn defaults_with_only_database_url() {
        let c =
            Config::from_vars(&vars(&[("DATABASE_URL", "postgres://u:p@127.0.0.1/db")])).unwrap();
        assert_eq!(c.port, 8080);
        assert_eq!(c.core_url, "http://127.0.0.1:8080");
        assert_eq!(c.nessie_mode, NessieMode::Mock);
        assert!(!c.demo_mode);
        assert_eq!(c.tax_rate_bps, 750);
        assert_eq!(c.ferry_fee_cents, 150);
        assert_eq!(c.demo_sim_speed, 8.0);
        assert!(c.supabase_secret_key.is_none());
        assert!(c.jwt_secret.is_none());
    }

    #[test]
    fn empty_means_unset() {
        let c = Config::from_vars(&vars(&[
            ("DATABASE_URL", "postgres://u:p@127.0.0.1/db"),
            ("SUPABASE_SECRET_KEY", ""),
            ("SUPABASE_URL", "  "),
            ("PORT", ""),
            ("NESSIE_MODE", ""),
        ]))
        .unwrap();
        assert!(c.supabase_secret_key.is_none());
        assert!(c.supabase_url.is_none());
        assert_eq!(c.port, 8080);
        assert_eq!(c.nessie_mode, NessieMode::Mock);
    }

    #[test]
    fn missing_database_url() {
        let err = Config::from_vars(&vars(&[("DATABASE_URL", "")])).unwrap_err();
        assert_eq!(err.to_string(), "DATABASE_URL is not set");
    }

    #[test]
    fn parses_values() {
        let c = Config::from_vars(&vars(&[
            ("DATABASE_URL", "postgres://u:p@127.0.0.1/db"),
            ("PORT", "8110"),
            ("NESSIE_MODE", "live"),
            ("DEMO_MODE", "TRUE"),
            ("TAX_RATE_BPS", "800"),
            ("DEMO_SIM_SPEED", "2.5"),
            ("SUPABASE_URL", "https://abc.supabase.co/"),
            ("PUBLIC_WEB_URL", "https://ferry.example"),
        ]))
        .unwrap();
        assert_eq!(c.port, 8110);
        assert_eq!(c.nessie_mode, NessieMode::Live);
        assert!(c.demo_mode);
        assert_eq!(c.tax_rate_bps, 800);
        assert_eq!(c.demo_sim_speed, 2.5);
        assert_eq!(c.supabase_url.as_deref(), Some("https://abc.supabase.co"));
        assert_eq!(c.telegram_bot_username, None);
        for (value, expected) in [
            ("Ferrygo_bot", Some("Ferrygo_bot")),
            ("@Ferrygo_bot", Some("Ferrygo_bot")),
            (" @Ferrygo_bot ", Some("Ferrygo_bot")),
            ("@", None),
        ] {
            let c = Config::from_vars(&vars(&[
                ("DATABASE_URL", "postgres://u:p@127.0.0.1/db"),
                ("TELEGRAM_BOT_USERNAME", value),
            ]))
            .unwrap();
            assert_eq!(c.telegram_bot_username.as_deref(), expected, "{value:?}");
        }
    }

    #[test]
    fn demo_telegram_personas() {
        let with = |pairs: &[(&str, &str)]| {
            let mut all = vec![("DATABASE_URL", "postgres://u:p@127.0.0.1/db")];
            all.extend_from_slice(pairs);
            Config::from_vars(&vars(&all)).unwrap()
        };
        let c = with(&[]);
        assert!(c.demo_telegram_personas.is_empty());
        assert!(c.summary().contains("DEMO_TELEGRAM_PERSONAS=unset"));

        let value = " maya=@Maya_Demo, judge=sal_tg ,leo=@leo_1,";
        let c = with(&[("DEMO_MODE", "true"), ("DEMO_TELEGRAM_PERSONAS", value)]);
        assert_eq!(
            c.demo_telegram_personas,
            vec![
                (DemoPersona::Maya, "Maya_Demo".to_owned()),
                (DemoPersona::Judge, "sal_tg".to_owned()),
                (DemoPersona::Leo, "leo_1".to_owned()),
            ]
        );
        assert_eq!(
            c.demo_persona_for_telegram("maya_demo"),
            Some(DemoPersona::Maya)
        );
        assert_eq!(
            c.demo_persona_for_telegram("@SAL_TG"),
            Some(DemoPersona::Judge)
        );
        assert_eq!(c.demo_persona_for_telegram("dana"), None);
        assert!(c.summary().contains("DEMO_TELEGRAM_PERSONAS=set"));
        assert!(!c.summary().contains("aya_"), "{}", c.summary());

        // Outside demo mode it is not read: one warning, valid or not, and no personas.
        let (c, logs) = capture_logs(|| with(&[("DEMO_TELEGRAM_PERSONAS", value)]));
        assert!(c.demo_telegram_personas.is_empty());
        assert_eq!(c.demo_persona_for_telegram("maya_demo"), None);
        assert_one_line(&logs, "WARN", "DEMO_MODE is off");
        assert!(!logs.contains("aya_"), "{logs}");
        assert!(c.summary().contains("DEMO_TELEGRAM_PERSONAS=ignored"));
        assert!(!c.summary().contains("aya_"), "{}", c.summary());

        // A value with no pairs is read and valid, so set, with no personas.
        let c = with(&[("DEMO_MODE", "true"), ("DEMO_TELEGRAM_PERSONAS", ",")]);
        assert!(c.demo_telegram_personas.is_empty());
        assert!(c.summary().contains("DEMO_TELEGRAM_PERSONAS=set"));

        // A valid value logs nothing.
        let (_, logs) =
            capture_logs(|| with(&[("DEMO_MODE", "true"), ("DEMO_TELEGRAM_PERSONAS", value)]));
        assert!(logs.is_empty(), "{logs}");

        // A malformed value never stops the server: with demo mode off it is ignored with one
        // warning, unparsed; with it on, it is one error line and no personas.
        for bad in [
            "maya",
            "mallory=@x_y",
            "maya=@",
            "maya=@has space",
            "maya=@a,maya=@b",
            "maya=@same,dana=@SAME",
            "maya=<username>",
        ] {
            let (c, logs) = capture_logs(|| with(&[("DEMO_TELEGRAM_PERSONAS", bad)]));
            assert!(c.demo_telegram_personas.is_empty(), "{bad}");
            assert!(!c.demo_mode);
            assert_one_line(&logs, "WARN", "DEMO_MODE is off");
            assert!(!logs.contains(bad), "{logs}");
            assert!(c.summary().contains("DEMO_TELEGRAM_PERSONAS=ignored"));
            assert!(!c.summary().contains(bad), "{}", c.summary());

            let (c, logs) =
                capture_logs(|| with(&[("DEMO_MODE", "true"), ("DEMO_TELEGRAM_PERSONAS", bad)]));
            assert!(c.demo_mode);
            assert!(c.demo_telegram_personas.is_empty(), "{bad}");
            assert_eq!(c.demo_persona_for_telegram("maya_demo"), None);
            assert_one_line(&logs, "ERROR", "DEMO_TELEGRAM_PERSONAS is invalid");
            assert!(logs.contains("demo personas are disabled"), "{logs}");
            assert!(!logs.contains(bad), "{logs}");
            assert!(c.summary().contains("DEMO_TELEGRAM_PERSONAS=invalid"));
            assert!(!c.summary().contains(bad), "{}", c.summary());
        }
    }

    /// Runs `f` with a subscriber that writes every event at any level into a string, and returns
    /// both.
    fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
        #[derive(Clone, Default)]
        struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let value = tracing::subscriber::with_default(subscriber, f);
        let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        (value, logs)
    }

    /// `logs` is exactly one line, at `level`, containing `text`.
    fn assert_one_line(logs: &str, level: &str, text: &str) {
        let lines: Vec<&str> = logs.lines().collect();
        assert_eq!(lines.len(), 1, "{logs}");
        assert!(
            lines[0].contains(level) && lines[0].contains(text),
            "{logs}"
        );
    }

    #[test]
    fn rejects_bad_values_without_echoing_them() {
        for (name, value) in [
            ("PORT", "80800"),
            ("NESSIE_MODE", "sandbox"),
            ("DEMO_MODE", "maybe"),
            ("FERRY_FEE_CENTS", "-1"),
            ("SUPABASE_URL", "not a url"),
        ] {
            let err = Config::from_vars(&vars(&[
                ("DATABASE_URL", "postgres://x@127.0.0.1/db"),
                (name, value),
            ]))
            .unwrap_err()
            .to_string();
            assert!(err.starts_with(name), "{err}");
            assert!(!err.contains(value), "{err}");
        }
    }

    #[test]
    fn debug_never_prints_secrets() {
        let c = Config::from_vars(&vars(&[
            ("DATABASE_URL", "postgres://u:hunter2@127.0.0.1/db"),
            ("SUPABASE_SECRET_KEY", "sb_secret_abc123"),
            ("JWT_SECRET", "jwt-shh"),
        ]))
        .unwrap();
        let printed = format!("{c:?} {}", c.summary());
        for secret in ["hunter2", "sb_secret_abc123", "jwt-shh"] {
            assert!(!printed.contains(secret), "{printed}");
        }
        assert!(c.summary().contains("SUPABASE_SECRET_KEY=set"));
        assert!(c.summary().contains("VAPID_PRIVATE_KEY=unset"));
    }

    #[test]
    fn finds_env_file_next_to_contract() {
        let root =
            std::env::temp_dir().join(format!("ferry-config-test-{}", crate::ids::new_id("t")));
        let nested = root.join("backend/ferry-core");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(root.join("contract")).unwrap();
        // A .env without contract/ beside it is not the root one.
        std::fs::write(root.join("backend/.env"), "PORT=1\n").unwrap();
        std::fs::write(
            root.join(".env"),
            "PORT=9000\nDATABASE_URL=postgres://f@127.0.0.1/db\n",
        )
        .unwrap();

        let found = find_env_file(None, &nested).unwrap();
        assert_eq!(found.as_deref(), Some(root.join(".env").as_path()));
        let file = read_env_file(&root.join(".env")).unwrap();
        assert_eq!(file.get("PORT").map(String::as_str), Some("9000"));

        // FERRY_ENV_FILE wins over the walk, and must exist.
        let explicit = root.join("backend/.env");
        assert_eq!(
            find_env_file(explicit.to_str(), &nested)
                .unwrap()
                .as_deref(),
            Some(explicit.as_path())
        );
        assert!(find_env_file(Some("/nonexistent/.env"), &nested).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn env_file_parse_error_hides_the_line() {
        let dir =
            std::env::temp_dir().join(format!("ferry-config-test-{}", crate::ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "OK=1\nJWT_SECRET='unterminated-secret-value\n").unwrap();
        let err = read_env_file(&path).unwrap_err().to_string();
        assert!(!err.contains("unterminated-secret-value"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_name_read_is_in_env_example() {
        let example =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example"))
                .unwrap();
        let documented: Vec<&str> = example
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
            .collect();
        for name in NAMES.iter().filter(|n| **n != ENV_FILE_VAR) {
            assert!(
                documented.contains(name),
                "{name} is read but not in .env.example"
            );
        }
        for name in &documented {
            if !NAMES.contains(name) {
                eprintln!("note: {name} is in .env.example but ferry-core does not read it");
            }
        }
    }
}
