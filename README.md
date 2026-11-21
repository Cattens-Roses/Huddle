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

## Media

Chat attachments support images plus MP4/M4V, MOV, MKV, WebM, MP3, M4A,
AAC, WAV, FLAC, and Ogg/OGA files, up to 20 MB. Use **Upload media** in a room
or thread, then **Play** on the message. Playback uses native Rust decoders,
GPUI video rendering, and system audio output, with Pause, Resume, and Close controls.

Video containers support H.264, 8-bit HEVC, and VP9 video with AAC, MP3, FLAC,
PCM, or Opus audio. Ogg audio supports Vorbis; M4A supports AAC. Containers can
hold other codecs, so the app decodes and validates files before accepting them
for upload. Unsupported codecs and unreadable files show an error.

On Linux, building requires ALSA development headers (`libasound2-dev` on Debian
and Ubuntu); playback requires an available system audio device. No browser,
external player, or FFmpeg installation is used for media playback.

## License

GPL-3.0-or-later. See [COPYING](./COPYING) and [THIRD-PARTY.md](./THIRD-PARTY.md) for license
and attribution details.
