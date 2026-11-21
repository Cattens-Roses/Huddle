"""Regenerate synthetic media fixtures: python generate.py /path/to/ffmpeg.

FFmpeg is a test fixture generator only; Huddle never launches it at runtime.
All fixture content is generated test patterns and sine waves.
"""
from pathlib import Path
import subprocess
import sys

root = Path(__file__).parent
ffmpeg = sys.argv[1] if len(sys.argv) > 1 else "ffmpeg"


def run(*args):
    subprocess.run([ffmpeg, "-hide_banner", "-loglevel", "error", "-y", *map(str, args)], check=True)


video = ["-f", "lavfi", "-i", "testsrc2=size=64x48:rate=12:duration=0.75",
         "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.75"]
run(*video, "-c:v", "libx264", "-profile:v", "baseline", "-pix_fmt", "yuv420p",
    "-c:a", "aac", "-b:a", "48k", "-shortest", root / "clip.mp4")
for extension in ["mov", "m4v", "mkv"]:
    run("-i", root / "clip.mp4", "-c", "copy", root / f"clip.{extension}")
run(*video, "-c:v", "libvpx-vp9", "-b:v", "80k", "-c:a", "libopus", "-b:a", "32k",
    "-shortest", root / "clip.webm")
run("-i", root / "clip.mp4", "-an", "-c:v", "copy", root / "silent.mp4")
run("-f", "lavfi", "-i", "testsrc2=size=96x64:rate=24:duration=1",
    "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p", "-bf", "2",
    "-g", "24", root / "reordered.mp4")
run("-i", root / "reordered.mp4", "-pix_fmt", "yuv420p", "-f", "rawvideo", root / "reordered.yuv")
run("-i", root / "reordered.mp4", "-c", "copy", root / "reordered.mkv")
run("-f", "lavfi", "-i", "testsrc2=size=64x48:rate=12:duration=12",
    "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p", "-bf", "2",
    "-g", "24", root / "seek.mp4")
run("-f", "lavfi", "-i", "sine=frequency=220:sample_rate=44100:duration=10",
    "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=44100:duration=2",
    "-filter_complex", "[0:a][1:a]concat=n=2:v=0:a=1[a]", "-map", "[a]",
    "-c:a", "aac", "-b:a", "48k", root / "seek-audio.m4a")
run("-i", root / "seek.mp4", "-i", root / "seek-audio.m4a",
    "-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-shortest", root / "seek-av.mp4")
for extension, codec in [("mp3", "libmp3lame"), ("m4a", "aac"), ("aac", "aac"),
                         ("wav", "pcm_s16le"), ("flac", "flac"), ("ogg", "libvorbis")]:
    run("-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=0.25",
        "-c:a", codec, root / f"tone.{extension}")
