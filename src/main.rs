mod api;
mod app;
mod auth;
mod browse;
mod cache;
mod config;
mod event;
mod player;
mod settings;
mod ui;

use anyhow::Result;
use app::App;
use event::Event;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    // reqwest (aws-lc-rs) and librespot (ring) both enable a provider, so rustls can't pick one.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;
    let paths = config::Paths::resolve()?;
    let cfg = config::Config::load(&paths)?;
    std::fs::create_dir_all(&paths.cache_dir)?;
    init_logging(&paths);

    // Interactive logins happen before the TUI takes over the terminal.
    let auth = auth::WebAuth::login(&cfg, &paths).await?;
    let api = api::Api::new(auth.clone())?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();

    let (pb_tx, mut pb_rx) = mpsc::unbounded_channel();
    println!("Connecting to Spotify...");
    let started = player::start(&cfg, &paths, pb_tx).await?;
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(e) = pb_rx.recv().await {
                if tx.send(Event::Playback(e)).is_err() {
                    break;
                }
            }
        });
    }
    let player = started.handle.clone();

    let mut terminal = ratatui::init(); // installs a panic hook that restores the terminal
    // Must run after entering the alternate screen and before the input task starts reading stdin.
    let mut picker = ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks());
    // herdr relays the outer terminal's capability replies (Kitty) but drops the graphics
    // itself, leaving a blank image; colored half-blocks are ordinary text and pass through.
    if std::env::var_os("HERDR_ENV").is_some() {
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Halfblocks);
    }
    event::spawn_input(tx.clone());
    let data_cache = cache::Cache::new(paths.cache_dir.join("data"));
    let settings = settings::Settings::new(cfg, paths.config_file.clone());
    let mut app = App::new(api, started.handle, data_cache, tx, started.volume, picker, settings);

    let result: Result<()> = async {
        while !app.quit {
            if app.dirty || app.anim.running() {
                app.dirty = false;
                terminal.draw(|f| ui::draw(f, &mut app))?;
            }
            // A running transition needs frames even when nothing else happens.
            let ev = if app.anim.running() {
                match tokio::time::timeout(ui::FRAME, rx.recv()).await {
                    Ok(ev) => ev,
                    Err(_) => continue,
                }
            } else {
                rx.recv().await
            };
            let Some(ev) = ev else { break };
            app.on_event(ev);
            // Drain everything already queued so bursts (page chunks) cost one redraw.
            while let Ok(ev) = rx.try_recv() {
                app.on_event(ev);
            }
        }
        Ok(())
    }
    .await;

    ratatui::restore();
    player.shutdown();
    // Give Spirc a moment to disconnect cleanly.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    result
}

fn init_logging(paths: &config::Paths) {
    let Some(dir) = paths.log_file.parent() else { return };
    let Some(name) = paths.log_file.file_name() else { return };
    let appender = tracing_appender::rolling::never(dir, name);
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(appender)
        .with_ansi(false)
        .init();
}
