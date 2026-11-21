# baseapp

A small, reusable desktop application shell built with Rust and GPUI. It includes a Home screen,
an Appearance settings page, and the shared UI components and themes in `crates/ui`.

## Run

```sh
cargo run -p baseapp
```

## Verify

```sh
cargo check -p baseapp
```

## Notifications

Open Settings and select **Notifications** to configure desktop alerts on this
device. Choose direct messages, room messages, mentions, thread replies, and
incoming calls. Message previews and taskbar attention can be turned off separately.
The master switch pauses all desktop alerts while keeping category preferences.
Settings are saved automatically and do not change room unread counts.

## Cache

Downloaded media uses the account's existing Matrix disk cache, with a 512 MB
retention limit and automatic cleanup after 30 days without access. Generated
video thumbnails are cached too. The UI keeps bounded caches for encoded media
and previews, evicting the least recently used entries first.

Open **Settings → Cache** to see memory usage and clear cached media. Clearing
keeps account data, messages, active playback, and pending uploads.

## Media

Chat attachments support images plus MP4/M4V, MOV, MKV, WebM, MP3, M4A,
AAC, WAV, FLAC, and Ogg/OGA files, up to 20 MB. Use **Upload media** in a room
or thread, then **Play** on the message. Playback uses native Rust decoders,
GPUI video rendering, and system audio output, with Pause/Resume, Replay,
volume up/down, Mute, and ±10-second skip controls and an elapsed-time display.
The timeline supports clicking to seek and dragging the playhead, with a time
preview while dragging. Releasing the playhead seeks once and preserves pause.
Video thumbnails appear in the composer and chat. Huddle uploads thumbnails for
other clients and generates previews locally when a message has no usable thumbnail.

Video containers support H.264, 8-bit HEVC, and VP9 video with AAC, MP3, FLAC,
PCM, or Opus audio. Ogg audio supports Vorbis; M4A supports AAC. Containers can
hold other codecs, so the app decodes and validates files before accepting them
for upload. Unsupported codecs and unreadable files show an error.

On Linux, building requires ALSA development headers (`libasound2-dev` on Debian
and Ubuntu); playback requires an available system audio device. No browser,
external player, or FFmpeg installation is used for media playback.

Media regression tests use synthetic FFmpeg-generated fixtures committed under
`crates/baseapp/tests/media`. Run `cargo test -p baseapp matrix::` to test formats,
thumbnails, playback timing, pause/close, and uploads/downloads against a local
mock Matrix server. The optional system audio smoke test is
`cargo test -p baseapp system_audio_and_video_playback_smoke -- --ignored`.
Playback regression tests also check B-frame presentation order against FFmpeg's
decoded reference and verify seeking while paused, volume limits, and mute.

## License

GPL-3.0-or-later. See [COPYING](./COPYING) and [THIRD-PARTY.md](./THIRD-PARTY.md) for license
and attribution details.
