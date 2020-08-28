use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_trait::async_trait;
use lazy_static::lazy_static;
use log::debug;
use matrix_sdk::{
    events::room::message::{MessageEvent, MessageEventContent, TextMessageEventContent},
    Client, ClientConfig, EventEmitter, SyncRoom, SyncSettings,
};
use rink_core::date;
use rink_core::gnu_units;
use rink_core::{CURRENCY_FILE, DATES_FILE, DEFAULT_FILE};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

lazy_static! {
    pub static ref RINK: Arc<Mutex<rink_core::Context>> =
        Arc::new(Mutex::new(load_rink().unwrap()));
}

#[derive(Error, Debug)]
enum Error {
    #[error("matrix error")]
    MatrixError(#[from] matrix_sdk::Error),
    #[error("error when parsing config file")]
    ConfigError(#[from] toml::de::Error),
}

#[derive(Debug, Deserialize)]
struct Config {
    matrix: MatrixConfig,
}

#[derive(Debug, Deserialize)]
struct MatrixConfig {
    homeserver: String,
    username: String,
    password: String,
    rooms: Vec<String>,
}

struct RinkMessageHandler {
    /// This clone of the `Client` will send requests to the server,
    /// while the other keeps us in sync with the server using `sync_forever`.
    client: Client,
}

impl RinkMessageHandler {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

fn oneline_eval(input: &str) -> Result<String, String> {
    let ctx = RINK.clone();
    let mut ctx = ctx.lock().unwrap();

    rink_core::one_line(&mut ctx, input)
}

#[async_trait]
impl EventEmitter for RinkMessageHandler {
    async fn on_room_message(&self, room: SyncRoom, event: &MessageEvent) {
        if let SyncRoom::Joined(room) = room {
            let msg_body = if let MessageEvent {
                content: MessageEventContent::Text(TextMessageEventContent { body: msg_body, .. }),
                ..
            } = event
            {
                msg_body.clone()
            } else {
                String::new()
            };

            if msg_body.starts_with(".c ") {
                let input = &msg_body[3..];
                let result = oneline_eval(input);

                let response = match result {
                    Ok(result) => format!("{} = {}", input.trim(), result),
                    Err(err) => format!("Error: {}", err),
                };

                let content = MessageEventContent::Text(TextMessageEventContent {
                    body: response.to_string(),
                    format: None,
                    formatted_body: None,
                    relates_to: None,
                });

                // we clone here to hold the lock for as little time as possible.
                let room_id = room.read().await.room_id.clone();

                self.client
                    // send our message to the room we found the "!party" command in
                    // the last parameter is an optional Uuid which we don't care about.
                    .room_send(&room_id, content, None)
                    .await
                    .unwrap();
            }
        }
    }
}

fn load_rink() -> Result<rink_core::Context, anyhow::Error> {
    let mut ctx = rink_core::Context::new();

    let units = {
        let mut iter = gnu_units::TokenIterator::new(&*DEFAULT_FILE.unwrap()).peekable();
        gnu_units::parse(&mut iter)
    };
    let dates = date::parse_datefile(&*DATES_FILE);
    let currency = {
        let mut iter = gnu_units::TokenIterator::new(&*CURRENCY_FILE).peekable();
        gnu_units::parse(&mut iter)
    };

    ctx.load(units);
    ctx.load_dates(dates);
    ctx.load(currency);

    Ok(ctx)
}

/// Loads the file at the given `path` and parses it as a TOML file according to `Config`
fn load_config<P: AsRef<Path>>(path: P) -> Result<Config, anyhow::Error> {
    let content = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;

    Ok(config)
}

async fn login_and_sync(
    homeserver_url: &str,
    username: &str,
    password: &str,
) -> Result<(), anyhow::Error> {
    let client_config = ClientConfig::new();
    let homeserver_url = Url::parse(&homeserver_url)
        .with_context(|| format!("failed to parse homeserver url: {}", homeserver_url))?;
    let mut client = Client::new_with_config(homeserver_url, client_config)?;

    client
        .login(username, password, None, Some("rust-sdk"))
        .await?;

    // Sync to skip old messages
    client.sync(SyncSettings::default()).await?;

    client
        .add_event_emitter(Box::new(RinkMessageHandler::new(client.clone())))
        .await;

    // Sync forever with our stored token
    let settings = SyncSettings::default().token(client.sync_token().await.unwrap());
    client.sync_forever(settings, |_| async {}).await;

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Load the config
    let config_path = "config.toml";
    let config = load_config(config_path)
        .with_context(|| format!("failed to load config file `{}'", config_path))?;

    debug!(
        "Logging in as {} on homeserver {}",
        config.matrix.username, config.matrix.homeserver
    );

    login_and_sync(
        &config.matrix.homeserver,
        &config.matrix.username,
        &config.matrix.password,
    )
    .await?;

    Ok(())
}
