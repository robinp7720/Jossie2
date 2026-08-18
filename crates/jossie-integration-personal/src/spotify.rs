use crate::{account_data, response_json, spec, token_field};
use base64::Engine;
use jossie_core::config::SpotifyConfig;
use jossie_core::integration::{Integration, OAuthAccount, OnboardingStatus, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

pub struct SpotifyIntegration {
    db: Arc<Database>,
    config: SpotifyConfig,
    client: reqwest::Client,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AccountArgs {
    account_id: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    account_id: String,
    query: String,
    #[schemars(required)]
    item_types: Option<Vec<String>>,
    #[schemars(required)]
    limit: Option<u32>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlayArgs {
    account_id: String,
    #[schemars(required)]
    device_id: Option<String>,
    #[schemars(required)]
    context_uri: Option<String>,
    #[schemars(required)]
    uris: Option<Vec<String>>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueueArgs {
    account_id: String,
    uri: String,
    #[schemars(required)]
    device_id: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaylistArgs {
    account_id: String,
    name: String,
    #[schemars(required)]
    description: Option<String>,
    #[schemars(required)]
    public: Option<bool>,
}

impl SpotifyIntegration {
    pub fn new(db: Arc<Database>, config: &SpotifyConfig) -> Self {
        Self {
            db,
            config: config.clone(),
            client: reqwest::Client::new(),
        }
    }
    async fn token(&self, id: &str) -> anyhow::Result<String> {
        let a = self
            .db
            .get_integration_account(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Spotify account not found"))?;
        anyhow::ensure!(a.integration == "spotify", "Account is not Spotify");
        let d = account_data(&a)?;
        if let Some(refresh) = d
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            let auth = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                self.config.client_id, self.config.client_secret
            ));
            let v = response_json(
                self.client
                    .post("https://accounts.spotify.com/api/token")
                    .header("Authorization", format!("Basic {auth}"))
                    .form(&[("grant_type", "refresh_token"), ("refresh_token", refresh)])
                    .send()
                    .await?,
                "Spotify token refresh",
            )
            .await?;
            return v
                .get("access_token")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("Spotify refresh returned no access token"));
        }
        let token = d
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        anyhow::ensure!(!token.is_empty(), "Spotify token is missing");
        Ok(token.to_string())
    }
    async fn request(
        &self,
        id: &str,
        method: reqwest::Method,
        url: String,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut r = self
            .client
            .request(method, url)
            .bearer_auth(self.token(id).await?);
        if let Some(b) = body {
            r = r.json(&b)
        }
        response_json(r.send().await?, "Spotify request").await
    }
}
#[async_trait::async_trait]
impl Integration for SpotifyIntegration {
    fn name(&self) -> &str {
        "spotify"
    }
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<SearchArgs>(
                "media_search",
                "Search Spotify for tracks, albums, artists, playlists, or shows.",
            ),
            ToolDefinition::for_args::<AccountArgs>(
                "media_now_playing",
                "Read the user's current Spotify playback.",
            ),
            ToolDefinition::for_args::<AccountArgs>(
                "media_get_queue",
                "Read the user's Spotify queue.",
            ),
            ToolDefinition::for_args::<PlayArgs>(
                "media_play",
                "Start or resume Spotify playback. Requires approval.",
            ),
            ToolDefinition::for_args::<AccountArgs>(
                "media_pause",
                "Pause Spotify playback. Requires approval.",
            ),
            ToolDefinition::for_args::<QueueArgs>(
                "media_add_to_queue",
                "Add one Spotify URI to the playback queue. Requires approval.",
            ),
            ToolDefinition::for_args::<PlaylistArgs>(
                "media_create_playlist",
                "Create a Spotify playlist. Requires approval.",
            ),
        ]
    }
    fn connection_spec(&self) -> Option<jossie_core::integration::ConnectionSpec> {
        Some(spec(
            "spotify",
            "Spotify",
            "Search music and control approved playback",
            vec![token_field()],
            !self.config.client_id.is_empty(),
        ))
    }
    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if self
            .db
            .list_integration_accounts("spotify")
            .await?
            .is_empty()
        {
            Ok(OnboardingStatus::RequiresAction { fields: Vec::new() })
        } else {
            Ok(OnboardingStatus::Configured)
        }
    }
    fn oauth_authorization_url(
        &self,
        redirect_uri: &str,
        state: &str,
    ) -> anyhow::Result<Option<String>> {
        if self.config.client_id.is_empty() {
            return Ok(None);
        }
        let scopes = "user-read-playback-state user-modify-playback-state playlist-modify-private playlist-modify-public";
        Ok(Some(format!(
            "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}",
            urlencoding::encode(&self.config.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(scopes),
            urlencoding::encode(state)
        )))
    }
    async fn oauth_exchange(&self, code: &str, redirect_uri: &str) -> anyhow::Result<OAuthAccount> {
        let auth = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            self.config.client_id, self.config.client_secret
        ));
        let v = response_json(
            self.client
                .post("https://accounts.spotify.com/api/token")
                .header("Authorization", format!("Basic {auth}"))
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", redirect_uri),
                ])
                .send()
                .await?,
            "Spotify OAuth exchange",
        )
        .await?;
        let access = v
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Spotify did not return an access token"))?;
        Ok(OAuthAccount {
            name: "Spotify account".into(),
            data: serde_json::json!({"access_token":access,"refresh_token":v.get("refresh_token").and_then(|v|v.as_str()).unwrap_or_default(),"source":"oauth"}),
        })
    }
    async fn execute(&self, name: &str, arguments: &str) -> anyhow::Result<String> {
        let v = match name {
            "media_search" => {
                let a: SearchArgs = serde_json::from_str(arguments)?;
                let types = a
                    .item_types
                    .unwrap_or_else(|| {
                        vec![
                            "track".into(),
                            "artist".into(),
                            "album".into(),
                            "playlist".into(),
                        ]
                    })
                    .join(",");
                self.request(
                    &a.account_id,
                    reqwest::Method::GET,
                    format!(
                        "https://api.spotify.com/v1/search?q={}&type={}&limit={}",
                        urlencoding::encode(&a.query),
                        urlencoding::encode(&types),
                        a.limit.unwrap_or(10).min(50)
                    ),
                    None,
                )
                .await?
            }
            "media_now_playing" => {
                let a: AccountArgs = serde_json::from_str(arguments)?;
                self.request(
                    &a.account_id,
                    reqwest::Method::GET,
                    "https://api.spotify.com/v1/me/player/currently-playing".into(),
                    None,
                )
                .await?
            }
            "media_get_queue" => {
                let a: AccountArgs = serde_json::from_str(arguments)?;
                self.request(
                    &a.account_id,
                    reqwest::Method::GET,
                    "https://api.spotify.com/v1/me/player/queue".into(),
                    None,
                )
                .await?
            }
            "media_play" => {
                let a: PlayArgs = serde_json::from_str(arguments)?;
                let mut b = serde_json::Map::new();
                if let Some(c) = a.context_uri {
                    b.insert("context_uri".into(), c.into());
                }
                if let Some(u) = a.uris {
                    b.insert("uris".into(), u.into());
                }
                let q = a
                    .device_id
                    .map(|d| format!("?device_id={}", urlencoding::encode(&d)))
                    .unwrap_or_default();
                self.request(
                    &a.account_id,
                    reqwest::Method::PUT,
                    format!("https://api.spotify.com/v1/me/player/play{q}"),
                    Some(b.into()),
                )
                .await?
            }
            "media_pause" => {
                let a: AccountArgs = serde_json::from_str(arguments)?;
                self.request(
                    &a.account_id,
                    reqwest::Method::PUT,
                    "https://api.spotify.com/v1/me/player/pause".into(),
                    None,
                )
                .await?
            }
            "media_add_to_queue" => {
                let a: QueueArgs = serde_json::from_str(arguments)?;
                let device = a
                    .device_id
                    .map(|d| format!("&device_id={}", urlencoding::encode(&d)))
                    .unwrap_or_default();
                self.request(
                    &a.account_id,
                    reqwest::Method::POST,
                    format!(
                        "https://api.spotify.com/v1/me/player/queue?uri={}{}",
                        urlencoding::encode(&a.uri),
                        device
                    ),
                    None,
                )
                .await?
            }
            "media_create_playlist" => {
                let a: PlaylistArgs = serde_json::from_str(arguments)?;
                let me = self
                    .request(
                        &a.account_id,
                        reqwest::Method::GET,
                        "https://api.spotify.com/v1/me".into(),
                        None,
                    )
                    .await?;
                let id = me
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Spotify profile has no id"))?;
                self.request(&a.account_id,reqwest::Method::POST,format!("https://api.spotify.com/v1/users/{}/playlists",urlencoding::encode(id)),Some(serde_json::json!({"name":a.name,"description":a.description.unwrap_or_default(),"public":a.public.unwrap_or(false)}))).await?
            }
            _ => anyhow::bail!("Unknown Spotify tool: {name}"),
        };
        Ok(serde_json::to_string_pretty(&v)?)
    }
}
