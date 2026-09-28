# spotify-tui

Fast Spotify terminal client. Rust, ratatui, embedded librespot (the app is its own Spotify Connect device; **Premium required**).

## Setup
1. Install Rust (`mise install`), plus ALSA dev headers.
2. `cargo run --release`. The browser opens once for a single Spotify login that covers both the Web API and the streaming device. Tokens are cached.

Optional: to use your own Web API rate limits, create an app at https://developer.spotify.com/dashboard (redirect URI `http://127.0.0.1:8888/callback`) and set `client_id` in `~/.config/spotify-tui/config.toml`. That requires a second one-time login for the streaming device.

## Keys
Press `?` in the app.

## Design
- `auth.rs` PKCE login + refreshing token cache (Web API). `player.rs` librespot session/Spirc; playback state comes from player events, not polling.
- `api/` Web API client (Feb 2026 endpoints: `/playlists/{id}/items`, search limit 10). Paged lists stream in chunks, fetched concurrently.
- `app.rs` state + input; `ui/` rendering; redraw only when state changed.
- Logs: `~/.cache/spotify-tui/spotify-tui.log`.
