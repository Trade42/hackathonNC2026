//! `GET /v1/public/config`: what a client needs before sign-in. Unauthenticated, so it carries
//! only public values: never the Supabase secret key, only the publishable one.

use axum::extract::State;
use serde::Serialize;

use crate::config::Config;
use crate::extract::Json;
use crate::state::AppState;

/// `web_url` when `PUBLIC_WEB_URL` is unset: the web app's dev server (.env.example).
pub const DEFAULT_WEB_URL: &str = "http://localhost:5173";

/// How the agent is shown without a number of its own: always, while messaging runs on Telegram
/// (contract v1.3).
pub const DEFAULT_AGENT_DISPLAY: &str = "Ferry";

/// The start parameter of `agent.telegram_add_to_group_url`, `https://t.me/<bot>?startgroup=ferry`.
/// Fixed and meaningless: a group added through the landing page's "Add Ferry to your group" is not
/// tied to an invite, unlike meet-again's `startgroup=<invite_id>` (docs/DECISIONS.md, auth-telegram).
pub const ADD_TO_GROUP_CODE: &str = "ferry";

/// `https://t.me/<bot>`, the bot's chat.
pub fn telegram_url(bot_username: &str) -> String {
    format!("https://t.me/{bot_username}")
}

/// `PublicConfig`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublicConfig {
    pub web_url: String,
    pub demo_mode: bool,
    pub agent: PublicConfigAgent,
    pub downloads: PublicConfigDownloads,
    pub supabase: Option<PublicConfigSupabase>,
}

/// `PublicConfigAgent`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublicConfigAgent {
    pub imessage_phone_e164: Option<String>,
    pub display: String,
    pub telegram_url: Option<String>,
    pub telegram_add_to_group_url: Option<String>,
    pub whatsapp_url: Option<String>,
}

/// `PublicConfigDownloads`: no native app builds yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublicConfigDownloads {
    pub macos: Option<String>,
    pub android: Option<String>,
    pub ios: Option<String>,
}

/// `PublicConfigSupabase`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublicConfigSupabase {
    pub url: String,
    pub publishable_key: String,
}

impl PublicConfig {
    pub fn from_config(config: &Config) -> Self {
        let bot = config.telegram_bot_username.as_deref();
        Self {
            web_url: config
                .public_web_url
                .clone()
                .unwrap_or_else(|| DEFAULT_WEB_URL.to_owned()),
            demo_mode: config.demo_mode,
            agent: PublicConfigAgent {
                // Contract v1.3: iMessage is off, so there is no number, even when
                // AGENT_PHONE_E164 is set, and the agent shows as its name.
                imessage_phone_e164: None,
                display: DEFAULT_AGENT_DISPLAY.to_owned(),
                telegram_url: bot.map(telegram_url),
                telegram_add_to_group_url: bot
                    .map(|bot| format!("{}?startgroup={ADD_TO_GROUP_CODE}", telegram_url(bot))),
                whatsapp_url: None,
            },
            downloads: PublicConfigDownloads {
                macos: None,
                android: None,
                ios: None,
            },
            supabase: match (&config.supabase_url, &config.supabase_publishable_key) {
                (Some(url), Some(key)) => Some(PublicConfigSupabase {
                    url: url.clone(),
                    publishable_key: key.clone(),
                }),
                _ => None,
            },
        }
    }
}

pub async fn get_public_config(State(state): State<AppState>) -> Json<PublicConfig> {
    Json(PublicConfig::from_config(&state.config))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let mut vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        vars.insert("DATABASE_URL".into(), "postgres://u:p@127.0.0.1/db".into());
        Config::from_vars(&vars).unwrap()
    }

    #[test]
    fn defaults() {
        let public = PublicConfig::from_config(&config(&[]));
        assert_eq!(
            serde_json::to_value(&public).unwrap(),
            serde_json::json!({
                "web_url": "http://localhost:5173",
                "demo_mode": false,
                "agent": {
                    "imessage_phone_e164": null,
                    "display": "Ferry",
                    "telegram_url": null,
                    "telegram_add_to_group_url": null,
                    "whatsapp_url": null
                },
                "downloads": {"macos": null, "android": null, "ios": null},
                "supabase": null
            })
        );
    }

    #[test]
    fn from_the_environment() {
        let public = PublicConfig::from_config(&config(&[
            ("PUBLIC_WEB_URL", "https://ferry.example"),
            ("DEMO_MODE", "true"),
            ("AGENT_PHONE_E164", "+19195550100"),
            ("AGENT_PHONE_DISPLAY", "(919) 555-0100"),
            ("SUPABASE_URL", "https://abc.supabase.co"),
            ("SUPABASE_PUBLISHABLE_KEY", "sb_publishable_test"),
            ("SUPABASE_SECRET_KEY", "sb_secret_never_sent"),
            ("TELEGRAM_BOT_USERNAME", "@Ferrygo_bot"),
        ]));
        assert_eq!(public.web_url, "https://ferry.example");
        assert!(public.demo_mode);
        // iMessage is off (contract v1.3): no number and no formatted number, even when set.
        assert_eq!(public.agent.imessage_phone_e164, None);
        assert_eq!(public.agent.display, "Ferry");
        assert_eq!(
            public.supabase,
            Some(PublicConfigSupabase {
                url: "https://abc.supabase.co".into(),
                publishable_key: "sb_publishable_test".into(),
            })
        );
        assert_eq!(
            public.agent.telegram_url.as_deref(),
            Some("https://t.me/Ferrygo_bot")
        );
        assert_eq!(
            public.agent.telegram_add_to_group_url.as_deref(),
            Some("https://t.me/Ferrygo_bot?startgroup=ferry")
        );
        let body = serde_json::to_string(&public).unwrap();
        assert!(!body.contains("sb_secret_never_sent"), "{body}");
        assert!(!body.contains("5550100"), "{body}");
    }

    #[test]
    fn supabase_needs_both_the_url_and_the_publishable_key() {
        for pairs in [
            &[("SUPABASE_URL", "https://abc.supabase.co")][..],
            &[("SUPABASE_PUBLISHABLE_KEY", "sb_publishable_test")][..],
        ] {
            assert_eq!(PublicConfig::from_config(&config(pairs)).supabase, None);
        }
    }
}
