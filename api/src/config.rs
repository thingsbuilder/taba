//! Runtime configuration read from environment variables (spec 6.2.2).
//! Only `GCP_PROJECT` is required in phase 0; the rest has defaults or is optional.

use std::fmt;
use std::str::FromStr;

/// A value that must never appear in logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub gcp_project: String,
    pub unico_url: String,
    pub domain: Option<String>,
    pub human_token_access_key: Option<Secret>,
    pub hmac_key: Option<Secret>,
    pub thumbnail_key: Option<Secret>,
    pub pilot_responsible: Option<String>,
    pub shelter_radius_m: u32,
    pub event_radius_km: u32,
    pub inactivity_days: u32,
    pub purge_after_days: u32,
    pub delivery_retention_days: u32,
    pub session_days: u32,
    pub searches_per_day: u32,
    /// The time zone whose midnight turns over the daily search quota (IANA name).
    pub tz_pilot: String,
    pub delivery_sample_rate: f64,
    pub anomaly_checkins_per_10min: u32,
    pub anomaly_rescues_per_10min: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "missing required environment variable {name}"),
            Self::Invalid(name) => write!(f, "invalid value for environment variable {name}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Takes the lookup as a parameter so tests do not touch the process environment.
    /// Empty values count as unset (`.env.example` lists every name with no value).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        let text = |name| get(name);
        let secret = |name| get(name).map(Secret);
        let delivery_sample_rate: f64 = number(&get, "DELIVERY_SAMPLE_RATE", "0.05")?;
        if !(0.0..=1.0).contains(&delivery_sample_rate) {
            return Err(ConfigError::Invalid("DELIVERY_SAMPLE_RATE"));
        }

        Ok(Self {
            gcp_project: text("GCP_PROJECT").ok_or(ConfigError::Missing("GCP_PROJECT"))?,
            unico_url: text("UNICO_URL")
                .unwrap_or_else(|| "https://humantoken.unico.app/client/v1".into()),
            domain: text("DOMAIN"),
            human_token_access_key: secret("HUMAN_TOKEN_ACCESS_KEY"),
            hmac_key: secret("HMAC_KEY"),
            thumbnail_key: secret("THUMBNAIL_KEY"),
            pilot_responsible: text("PILOT_RESPONSIBLE"),
            shelter_radius_m: number(&get, "SHELTER_RADIUS_M", "100")?,
            event_radius_km: number(&get, "EVENT_RADIUS_KM", "50")?,
            inactivity_days: positive(number(&get, "INACTIVITY_DAYS", "30")?, "INACTIVITY_DAYS")?,
            purge_after_days: number(&get, "PURGE_AFTER_DAYS", "30")?,
            delivery_retention_days: number(&get, "DELIVERY_RETENTION_DAYS", "90")?,
            session_days: number(&get, "SESSION_DAYS", "7")?,
            searches_per_day: number(&get, "SEARCHES_PER_DAY", "10")?,
            tz_pilot: time_zone(text("TZ_PILOT"))?,
            delivery_sample_rate,
            anomaly_checkins_per_10min: number(&get, "ANOMALY_CHECKINS_PER_10MIN", "30")?,
            anomaly_rescues_per_10min: number(&get, "ANOMALY_RESCUES_PER_10MIN", "100")?,
        })
    }
}

/// `TZ_PILOT`: an IANA time zone name the server knows; São Paulo when absent.
fn time_zone(value: Option<String>) -> Result<String, ConfigError> {
    let name = value.unwrap_or_else(|| "America/Sao_Paulo".into());
    jiff::tz::TimeZone::get(name.trim())
        .map(|_| name.trim().to_string())
        .map_err(|_| ConfigError::Invalid("TZ_PILOT"))
}

/// An event that closes after zero idle days would close the moment it opens.
fn positive(days: u32, name: &'static str) -> Result<u32, ConfigError> {
    if days == 0 {
        Err(ConfigError::Invalid(name))
    } else {
        Ok(days)
    }
}

fn number<T: FromStr>(
    get: &dyn Fn(&str) -> Option<String>,
    name: &'static str,
    default: &str,
) -> Result<T, ConfigError> {
    let value = get(name).unwrap_or_else(|| default.into());
    value.trim().parse().map_err(|_| ConfigError::Invalid(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<_, _> = vars.iter().copied().collect();
        Config::from_lookup(|name| map.get(name).map(|v| v.to_string()))
    }

    #[test]
    fn only_gcp_project_is_required() {
        let config = load(&[("GCP_PROJECT", "p")]).unwrap();
        assert_eq!(config.gcp_project, "p");
        assert_eq!(config.unico_url, "https://humantoken.unico.app/client/v1");
        assert_eq!(config.shelter_radius_m, 100);
        assert_eq!(config.event_radius_km, 50);
        assert_eq!(config.inactivity_days, 30);
        assert_eq!(config.purge_after_days, 30);
        assert_eq!(config.delivery_retention_days, 90);
        assert_eq!(config.session_days, 7);
        assert_eq!(config.searches_per_day, 10);
        assert_eq!(config.tz_pilot, "America/Sao_Paulo");
        assert_eq!(config.delivery_sample_rate, 0.05);
        assert_eq!(config.anomaly_checkins_per_10min, 30);
        assert_eq!(config.anomaly_rescues_per_10min, 100);
        assert!(config.hmac_key.is_none() && config.domain.is_none());
    }

    #[test]
    fn missing_or_blank_gcp_project_fails() {
        assert_eq!(load(&[]), Err(ConfigError::Missing("GCP_PROJECT")));
        assert_eq!(
            load(&[("GCP_PROJECT", "  ")]),
            Err(ConfigError::Missing("GCP_PROJECT"))
        );
    }

    #[test]
    fn overrides_are_parsed() {
        let config = load(&[
            ("GCP_PROJECT", "p"),
            ("UNICO_URL", "http://127.0.0.1:9999"),
            ("SHELTER_RADIUS_M", "250"),
            ("DELIVERY_SAMPLE_RATE", "0.5"),
        ])
        .unwrap();
        assert_eq!(config.unico_url, "http://127.0.0.1:9999");
        assert_eq!(config.shelter_radius_m, 250);
        assert_eq!(config.delivery_sample_rate, 0.5);
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let config = load(&[("GCP_PROJECT", "p"), ("SESSION_DAYS", "")]).unwrap();
        assert_eq!(config.session_days, 7);
    }

    #[test]
    fn invalid_numbers_are_rejected() {
        assert_eq!(
            load(&[("GCP_PROJECT", "p"), ("SESSION_DAYS", "abc")]),
            Err(ConfigError::Invalid("SESSION_DAYS"))
        );
        assert_eq!(
            load(&[("GCP_PROJECT", "p"), ("SHELTER_RADIUS_M", "-1")]),
            Err(ConfigError::Invalid("SHELTER_RADIUS_M"))
        );
        assert_eq!(
            load(&[("GCP_PROJECT", "p"), ("TZ_PILOT", "Mars/Olympus")]),
            Err(ConfigError::Invalid("TZ_PILOT"))
        );
        assert_eq!(
            load(&[("GCP_PROJECT", "p"), ("TZ_PILOT", "Europe/Lisbon")])
                .unwrap()
                .tz_pilot,
            "Europe/Lisbon"
        );
        assert_eq!(
            load(&[("GCP_PROJECT", "p"), ("INACTIVITY_DAYS", "0")]),
            Err(ConfigError::Invalid("INACTIVITY_DAYS"))
        );
        assert_eq!(
            load(&[("GCP_PROJECT", "p"), ("DELIVERY_SAMPLE_RATE", "1.5")]),
            Err(ConfigError::Invalid("DELIVERY_SAMPLE_RATE"))
        );
    }

    #[test]
    fn secrets_are_redacted_in_debug_output() {
        let config = load(&[("GCP_PROJECT", "p"), ("HMAC_KEY", "super-secret")]).unwrap();
        assert!(!format!("{config:?}").contains("super-secret"));
    }
}
