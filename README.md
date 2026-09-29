# spotify-tui

Fast Spotify terminal client. Rust, ratatui, embedded librespot (the app is its own Spotify Connect device; **Premium required**).

## Setup
1. Install Rust (`mise install`), plus ALSA dev headers.
2. Create an app at https://developer.spotify.com/dashboard with redirect URI `http://127.0.0.1:8888/callback`.
3. `cargo run --release` writes `~/.config/spotify-tui/config.toml`; put your Client ID in `client_id` and run again.
4. Two one-time browser logins: your app (Web API, PKCE) and the streaming device (librespot's client id, which Spotify requires for streaming but rate-limits heavily on the Web API, so it is never used for browsing). Tokens are cached.

## Keys
Press `?` in the app.

## Design
- `auth.rs` PKCE login + refreshing token cache (Web API). `player.rs` librespot session/Spirc; playback state comes from player events, not polling.
- `api/` Web API client (Feb 2026 endpoints: `/playlists/{id}/items`, search limit 10). Paged lists stream in chunks, fetched concurrently.
- `app.rs` state + input; `ui/` rendering; redraw only when state changed, plus ~60 fps for the 180 ms of a pane transition (`ui/anim.rs`: contents glide in and un-dim, no colour fades, so any theme works).
- `browse.rs` Browse page: Spotify's live "Browse all" catalogue (undocumented pathfinder GraphQL persisted queries, authorised with the streaming session's login5 + client token): categories → shelves → every playlist/album, sub-pages included; plus radio stations. `cache.rs` disk cache (`~/.cache/spotify-tui/data`): browse pages and top artists use TTLs, playlists and track lists paint from cache instantly then refresh (skipping the network if under 5 minutes old).
- Logs: `~/.cache/spotify-tui/spotify-tui.log`.
