use crate::config::Config;
use crate::status::GhostwriterStatus;
use crate::touch::{Touch, TriggerCorner};
use anyhow::Result;
use log::{info, warn};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::{Arc, RwLock};
use warp::http::StatusCode;
use warp::reply::{json as reply_json, with_status};
use warp::{Filter, Rejection, Reply};

const WEB_FILES: &[(&str, &str, &str)] = &[
    ("index.html", include_str!("web/index.html"), "text/html"),
    ("style.css", include_str!("web/style.css"), "text/css"),
    ("app.js", include_str!("web/app.js"), "application/javascript"),
];

/// Start the web server on the specified port with shared state
pub async fn start_web_server(
    port: u16,
    shared_config: Arc<RwLock<Config>>,
    shared_status: Arc<RwLock<GhostwriterStatus>>,
    shared_touch: Option<Arc<RwLock<Touch>>>,
) -> Result<()> {
    info!("Starting web server on port {}", port);

    // Static file routes
    let static_files = warp::path::end()
        .map(|| serve_static_file("index.html"))
        .or(warp::path("style.css").map(|| serve_static_file("style.css")))
        .or(warp::path("app.js").map(|| serve_static_file("app.js")));

    // API routes with shared state
    let config_for_get = Arc::clone(&shared_config);
    let config_for_post = Arc::clone(&shared_config);
    let status_for_get = Arc::clone(&shared_status);

    let api_routes = warp::path("api").and(
        // GET /api/config
        warp::path("config")
            .and(warp::get())
            .and(warp::any().map(move || Arc::clone(&config_for_get)))
            .and_then(get_config_handler)
            .or(
                // POST /api/config
                warp::path("config")
                    .and(warp::post())
                    .and(warp::body::json())
                    .and(warp::any().map(move || Arc::clone(&config_for_post)))
                    .and_then(save_config_handler),
            )
            .or(
                // GET /api/status
                warp::path("status")
                    .and(warp::get())
                    .and(warp::any().map(move || Arc::clone(&status_for_get)))
                    .and_then(get_status_handler),
            )
            .or(
                // Simulation endpoints
                warp::path("simulation").and(
                    // POST /api/simulation/trigger
                    warp::path("trigger")
                        .and(warp::post())
                        .and(warp::body::json())
                        .and(warp::any().map(move || shared_touch.clone()))
                        .and_then(simulation_trigger_handler),
                ),
            ),
    );

    // CORS headers for API
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST", "OPTIONS"]);

    let routes = static_files.or(api_routes).with(cors).recover(handle_rejection);

    info!("Web interface available at http://localhost:{}/", port);

    warp::serve(routes).run(([0, 0, 0, 0], port)).await;

    Ok(())
}

fn serve_static_file(filename: &str) -> impl Reply {
    if let Some((_, content, content_type)) = WEB_FILES.iter().find(|(name, _, _)| *name == filename) {
        warp::reply::with_header(warp::reply::with_status(*content, StatusCode::OK), "content-type", *content_type)
    } else {
        warp::reply::with_header(warp::reply::with_status("File not found", StatusCode::NOT_FOUND), "content-type", "text/plain")
    }
}

async fn simulation_trigger_handler(trigger_data: Value, shared_touch: Option<Arc<RwLock<Touch>>>) -> Result<impl Reply, Rejection> {
    let corner_str = trigger_data["corner"].as_str().unwrap_or("UR");

    if let Some(touch_arc) = shared_touch {
        match TriggerCorner::from_string(corner_str) {
            Ok(corner) => match touch_arc.read() {
                Ok(touch) => {
                    touch.add_manual_trigger(corner);
                    info!("Manual trigger added for corner: {:?}", corner);
                    Ok(reply_json(&json!({
                        "status": "success",
                        "message": format!("Trigger added for corner: {:?}", corner)
                    })))
                }
                Err(e) => {
                    warn!("Failed to access touch component: {}", e);
                    Err(warp::reject::custom(ConfigError::LoadFailed(e.to_string())))
                }
            },
            Err(e) => {
                warn!("Invalid trigger corner: {}", e);
                Err(warp::reject::custom(ConfigError::ValidationFailed(e.to_string())))
            }
        }
    } else {
        warn!("Simulation endpoints not available: no touch component");
        Err(warp::reject::custom(ConfigError::ValidationFailed(
            "Simulation endpoints are only available when touch component is enabled".to_string(),
        )))
    }
}

async fn get_config_handler(shared_config: Arc<RwLock<Config>>) -> Result<impl Reply, Rejection> {
    match shared_config.read() {
        Ok(config) => Ok(reply_json(&*config)),
        Err(e) => {
            warn!("Failed to read shared config: {}", e);
            Err(warp::reject::custom(ConfigError::LoadFailed(e.to_string())))
        }
    }
}

async fn save_config_handler(config: Config, shared_config: Arc<RwLock<Config>>) -> Result<impl Reply, Rejection> {
    // Validate the config before saving
    if let Err(e) = config.validate() {
        warn!("Config validation failed: {}", e);
        return Err(warp::reject::custom(ConfigError::ValidationFailed(e.to_string())));
    }

    // Update shared config first (for immediate effect)
    match shared_config.write() {
        Ok(mut shared) => {
            *shared = config.clone();
        }
        Err(e) => {
            warn!("Failed to update shared config: {}", e);
            return Err(warp::reject::custom(ConfigError::SaveFailed(e.to_string())));
        }
    }

    // Also save to file
    match config.save() {
        Ok(()) => {
            info!("Configuration saved successfully and updated in memory");
            Ok(reply_json(&json!({
                "status": "success",
                "message": "Configuration saved successfully and applied immediately"
            })))
        }
        Err(e) => {
            warn!("Failed to save config to file: {}", e);
            Err(warp::reject::custom(ConfigError::SaveFailed(e.to_string())))
        }
    }
}

async fn get_status_handler(shared_status: Arc<RwLock<GhostwriterStatus>>) -> Result<impl Reply, Rejection> {
    match shared_status.read() {
        Ok(status) => Ok(reply_json(&*status)),
        Err(e) => {
            warn!("Failed to read shared status: {}", e);
            Err(warp::reject::custom(ConfigError::LoadFailed(e.to_string())))
        }
    }
}

#[derive(Debug)]
enum ConfigError {
    LoadFailed(String),
    SaveFailed(String),
    ValidationFailed(String),
}

impl warp::reject::Reject for ConfigError {}

async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let (code, message) = if err.is_not_found() {
        (StatusCode::NOT_FOUND, "Not Found".to_string())
    } else if let Some(config_err) = err.find::<ConfigError>() {
        match config_err {
            ConfigError::LoadFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load config: {}", msg)),
            ConfigError::SaveFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save config: {}", msg)),
            ConfigError::ValidationFailed(msg) => (StatusCode::BAD_REQUEST, format!("Config validation failed: {}", msg)),
        }
    } else if err.find::<warp::filters::body::BodyDeserializeError>().is_some() {
        (StatusCode::BAD_REQUEST, "Invalid JSON".to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error".to_string())
    };

    let json = reply_json(&json!({
        "error": message,
        "code": code.as_u16()
    }));

    Ok(with_status(json, code))
}
