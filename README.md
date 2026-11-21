<p align="center">
  <img src="/assets/tray/sonora.png" alt="Huddle" width="128" height="128">
</p>

<h1 align="center">Huddle</h1>

<p align="center">
  <strong>Native Matrix desktop client</strong><br>
  Encrypted chat, rooms, threads, calls, and media - built with Rust and GPUI.
</p>

<p align="center">
  <a href="https://github.com/Cattens-Roses/Huddle/releases/latest"><img src="https://img.shields.io/github/v/release/Cattens-Roses/Huddle?style=flat-square&label=release" alt="Release"></a>
  <a href="https://github.com/Cattens-Roses/Huddle/stargazers"><img src="https://img.shields.io/github/stars/Cattens-Roses/Huddle?style=flat-square" alt="Stars"></a>
  <a href="https://github.com/Cattens-Roses/Huddle/network/members"><img src="https://img.shields.io/github/forks/Cattens-Roses/Huddle?style=flat-square" alt="Forks"></a>
  <a href="https://github.com/Cattens-Roses/Huddle/issues"><img src="https://img.shields.io/github/issues/Cattens-Roses/Huddle?style=flat-square" alt="Issues"></a>
  <a href="https://github.com/Cattens-Roses/Huddle/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Cattens-Roses/Huddle/ci.yml?style=flat-square&label=ci" alt="CI"></a>
  <a href="COPYING"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue?style=flat-square" alt="License"></a>
</p>

## Features

- **Rooms & DMs** - join rooms, message people directly, and follow threads
- **Encryption** - end-to-end encrypted messaging with Matrix
- **Calls** - voice and video calling from the desktop client
- **Native media** - upload and play images plus common audio/video formats without a browser or FFmpeg install
- **Notifications** - per-category desktop alerts, previews, and taskbar attention
- **Cache** - disk and memory media caches with size limits and one-click clear
- **Appearance** - customizable UI using the shared GPUI design system
- **Desktop extras** - system tray, background presence, and packaged Linux/Windows builds
- **Auto-updates** - Windows in-app differential updates; Linux apt repository updates

## Install

### Linux (apt)

```sh
echo "deb [trusted=yes] https://cattens-roses.github.io/Huddle ./" \
  | sudo tee /etc/apt/sources.list.d/huddle.list
sudo apt update
sudo apt install huddle
```

Details: [cattens-roses.github.io/Huddle](https://cattens-roses.github.io/Huddle/).

### Windows

Install [Huddle-Setup.exe](https://github.com/Cattens-Roses/Huddle/releases/tag/beta) (per-user). The app checks the rolling `beta` channel and applies changed files in place.

### Beta builds

Every push to `main` refreshes the rolling [beta](https://github.com/Cattens-Roses/Huddle/releases/tag/beta) prerelease (`.deb`, Windows setup/exe, and update blobs).

## How to run

```sh
cargo run -p baseapp
```

1. Launch Huddle.
2. Sign in to your Matrix homeserver.
3. Open rooms, DMs, or threads and chat as usual.
4. Optional: configure **Settings → Notifications**, **Cache**, **Updates**, and appearance.

## Building from source

Requires a recent Rust toolchain (see `rust-toolchain.toml`) and platform UI/media dependencies.

```sh
cargo check -p baseapp
cargo run -p baseapp
cargo test -p baseapp matrix::
```

On Linux, install ALSA development headers for playback (e.g. `libasound2-dev` on Debian/Ubuntu), plus the usual X11/Wayland and fontconfig build packages used by GPUI.

Media regression tests use fixtures under `crates/baseapp/tests/media`. An optional system-audio smoke test:

```sh
cargo test -p baseapp system_audio_and_video_playback_smoke -- --ignored
```

## Project layout

- `crates/baseapp` - app shell, Matrix client, media, calls, settings, updater
- `crates/ui` - GPUI components and theme tokens
- `crates/icons` / `crates/i18n` / `crates/embed` - icons, localization, embedding helpers
- `assets` - app icon and UI icon sets
- `packaging` - Linux `.deb`, Windows installer, and update manifest scripts
- `flatpak` / `nix` - packaging and Nix flake support
- `.github` - CI, beta release, apt repo publish, issue templates

## Media notes

Chat attachments support images plus MP4/M4V, MOV, MKV, WebM, MP3, M4A, AAC, WAV, FLAC, and Ogg/OGA (up to 20 MB). Playback uses native Rust decoders, GPUI video rendering, and system audio. Video containers support H.264, 8-bit HEVC, and VP9 with AAC, MP3, FLAC, PCM, or Opus audio.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please follow the
[Code of Conduct](CODE_OF_CONDUCT.md). Security reports go through
[SECURITY.md](SECURITY.md), not public issues. Privacy notes are in
[PRIVACY.md](PRIVACY.md).

## License

GPL-3.0-or-later. See [COPYING](./COPYING) and [THIRD-PARTY.md](./THIRD-PARTY.md).
