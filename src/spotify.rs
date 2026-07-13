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
            .send().await?.error_for_status()?
            .json::<TokenResponse>().await?;
        Ok(resp.access_token)
    }

    /// Find the first track matching a query. Returns its URI.
    async fn find_track(&self, token: &str, query: &str) -> Result<String> {
        let search: serde_json::Value = reqwest::Client::new()
            .get("https://api.spotify.com/v1/search")
            .bearer_auth(token)
            .query(&[("q", query), ("type", "track"), ("limit", "1")])
            .send().await?.error_for_status()?.json().await?;

        search["tracks"]["items"][0]["uri"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("no track found for '{}'", query))
    }

    /// Play a playlist by name. Searches the user's own playlists first.
    pub async fn play_playlist(&self, name: &str) -> Result<()> {
        let token = self.access_token().await?;
        let client = reqwest::Client::new();

        let resp: serde_json::Value = client
            .get("https://api.spotify.com/v1/me/playlists")
            .bearer_auth(&token)
            .query(&[("limit", "50")])
            .send().await?.error_for_status()?.json().await?;

        let empty = vec![];
        let items = resp["items"].as_array().unwrap_or(&empty);

        let wanted = name.to_lowercase();
        let uri = items.iter()
            .find(|p| p["name"].as_str()
                .map(|n| n.to_lowercase().contains(&wanted))
                .unwrap_or(false))
            .and_then(|p| p["uri"].as_str())
            .ok_or_else(|| anyhow::anyhow!("no playlist matching '{}'", name))?;

        log::info!("Playing playlist: {}", uri);
        client
            .put("https://api.spotify.com/v1/me/player/play")
            .bearer_auth(&token)
            .json(&serde_json::json!({ "context_uri": uri }))
            .send().await?.error_for_status()?;
        Ok(())
    }

    /// Play the first track, then queue the rest in order.
    pub async fn queue_tracks(&self, queries: &[String]) -> Result<()> {
        if queries.is_empty() {
            return Ok(());
        }
        let token = self.access_token().await?;
        let client = reqwest::Client::new();

        let mut uris = Vec::new();
        for q in queries {
            match self.find_track(&token, q).await {
                Ok(uri) => uris.push(uri),
                Err(e) => log::warn!("skipping '{}': {}", q, e),
            }
        }
        if uris.is_empty() {
            anyhow::bail!("none of the requested tracks were found");
        }

        // Start the first one immediately
        client
            .put("https://api.spotify.com/v1/me/player/play")
            .bearer_auth(&token)
            .json(&serde_json::json!({ "uris": [&uris[0]] }))
            .send().await?.error_for_status()?;

        // Queue the remainder
        for uri in &uris[1..] {
            client
                .post("https://api.spotify.com/v1/me/player/queue")
                .bearer_auth(&token)
                .query(&[("uri", uri)])
                .header("Content-Length", "0")
                .body("")
                .send().await?.error_for_status()?;
        }

        log::info!("Queued {} tracks", uris.len());
        Ok(())
    }

    pub async fn control(&self, action: &str) -> Result<()> {
        let token = self.access_token().await?;
        let client = reqwest::Client::new();
        let base = "https://api.spotify.com/v1/me/player";
        let resp = match action {
            "play"  => client.put(format!("{}/play", base)).bearer_auth(&token).send().await?,
            "pause" => client.put(format!("{}/pause", base)).bearer_auth(&token).send().await?,
            "next"  => client.post(format!("{}/next", base)).bearer_auth(&token)
                             .header("Content-Length", "0").body("").send().await?,
            "previous" => client.post(format!("{}/previous", base)).bearer_auth(&token)
                             .header("Content-Length", "0").body("").send().await?,
            other => anyhow::bail!("unknown spotify action: {}", other),
        };
        resp.error_for_status()?;
        Ok(())
    }
}
