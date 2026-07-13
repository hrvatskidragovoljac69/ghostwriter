use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

pub struct Spotify {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

impl Spotify {
    pub fn from_env() -> Option<Self> {
        Some(Self {
            client_id: std::env::var("SPOTIFY_CLIENT_ID").ok()?,
            client_secret: std::env::var("SPOTIFY_CLIENT_SECRET").ok()?,
            refresh_token: std::env::var("SPOTIFY_REFRESH_TOKEN").ok()?,
        })
    }

    async fn access_token(&self) -> Result<String> {
        let resp = reqwest::Client::new()
            .post("https://accounts.spotify.com/api/token")
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.refresh_token.as_str()),
            ])
            .send().await?
            .error_for_status()?
            .json::<TokenResponse>().await?;
        Ok(resp.access_token)
    }

    pub async fn search_and_play(&self, query: &str) -> Result<()> {
        let token = self.access_token().await?;
        let client = reqwest::Client::new();

        let search: serde_json::Value = client
            .get("https://api.spotify.com/v1/search")
            .bearer_auth(&token)
            .query(&[("q", query), ("type", "track"), ("limit", "1")])
            .send().await?.error_for_status()?.json().await?;

        let uri = search["tracks"]["items"][0]["uri"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no track found for '{}'", query))?
            .to_string();

        client
            .put("https://api.spotify.com/v1/me/player/play")
            .bearer_auth(&token)
            .json(&serde_json::json!({ "uris": [uri] }))
            .send().await?.error_for_status()?;
        Ok(())
    }

    pub async fn control(&self, action: &str) -> Result<()> {
        let token = self.access_token().await?;
        let client = reqwest::Client::new();
        let base = "https://api.spotify.com/v1/me/player";
        let resp = match action {
            "play"     => client.put(format!("{}/play", base)).bearer_auth(&token).send().await?,
            "pause"    => client.put(format!("{}/pause", base)).bearer_auth(&token).send().await?,
            "next"     => client.post(format!("{}/next", base)).bearer_auth(&token).header("Content-Length", "0").body("").send().await?,
            "previous" => client.post(format!("{}/previous", base)).bearer_auth(&token).header("Content-Length", "0").body("").send().await?,
            other => anyhow::bail!("unknown spotify action: {}", other),
        };
        resp.error_for_status()?;
        Ok(())
    }
}
