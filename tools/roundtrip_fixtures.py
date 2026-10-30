"""Generate three small source streams once and reuse qualified profile graphs."""
import hashlib
import shutil


def partial_audio_duration(rate):
    """Preserve the sample-derived f64 exactly, with unambiguous precision."""
    text = repr(240 / rate)
    if "." not in text:
        text += "."
    while len(text.split(".")[1]) <= 9:
        text += "0"
    return text


def source_seeds(root, clip, runner, support):
    """FFmpeg is confined to generating the attributed fixture, never conversion checks."""
    support.require(hashlib.sha256(clip).hexdigest() == support.CLIP_SHA256, "fixture-identity-mismatch")
    clip_path = root / "clip.mp4"
    support.private_write(clip_path, clip)
    streams = root / "streams"
    streams.mkdir(mode=0o700)
    for name, options in (("video", ["-map", "0:v:0", "-an", "-c:v", "libx264", "-preset", "fast",
                                    "-g", "48", "-keyint_min", "48", "-sc_threshold", "0", "-bf", "2"]),
                          ("aac44100", ["-map", "0:a:0", "-vn", "-c:a", "aac", "-ar", "44100", "-ac", "2"]),
                          ("aac48000", ["-map", "0:a:0", "-vn", "-c:a", "aac", "-ar", "48000", "-ac", "2"])):
        directory = streams / name
        directory.mkdir(mode=0o700)
        runner.run(["ffmpeg", "-v", "error", "-nostdin", "-threads", "1", "-i", clip_path,
                    *options, "-threads", "1", "-hls_time", "2", "-hls_playlist_type", "vod",
                    "-hls_segment_filename", directory / "segment-%03d.ts", directory / "media.m3u8"],
                   "fixture-" + name)
        if name == "video":
            probe = runner.json(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets",
                                 "-show_entries", "stream=codec_name,has_b_frames,nb_read_packets", "-of", "json",
                                 directory / "media.m3u8"], "fixture-probe-video")
            stream = probe.get("streams", [{}])[0]
            support.require(stream.get("codec_name") == "h264" and stream.get("has_b_frames", 0) >= 1
                            and stream.get("nb_read_packets") == "240", "fixture-video-profile-invalid")
        else:
            rate, frames = (44100, 432) if name == "aac44100" else (48000, 470)
            playlist = directory / "media.m3u8"
            text = playlist.read_text(encoding="utf-8").splitlines()
            media = [index for index, line in enumerate(text) if line and not line.startswith("#")]
            support.require(len(media) == 6 and text[media[-1] - 1].startswith("#EXTINF:"), "fixture-segments-invalid")
            # A complete final encoded frame has 240 presented samples. The
            # remaining coded samples must survive every container conversion.
            text[media[-1] - 1] = f"#EXTINF:{partial_audio_duration(rate)},"
            playlist.write_text("\n".join(text) + "\n", encoding="utf-8")
            transport = b"".join(support.read_regular(directory / text[index], 8 * 1024 * 1024) for index in media)
            probe_path = root / (name + ".ts")
            support.private_write(probe_path, transport)
            probe = runner.json(["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_packets", "-show_streams",
                                 "-show_entries", "stream=codec_name,sample_rate,channels:packet=pts,dts,duration",
                                 "-of", "json", probe_path], "fixture-probe-" + name)
            packets = probe.get("packets", [])
            support.require(probe.get("streams", [{}])[0].get("sample_rate") == str(rate)
                            and len(packets) == frames and all(type(packet.get("pts")) is int for packet in packets),
                            "fixture-audio-clock-invalid")
    seeds = root / "seeds"
    seeds.mkdir(mode=0o700)
    result = {}
    for layout in ("video", "aac44100", "aac48000", "video-aac44100", "video-aac48000"):
        destination = seeds / layout
        destination.mkdir(mode=0o700)
        if layout.startswith("video-"):
            audio = layout.split("-", 1)[1]
            shutil.copytree(streams / "video", destination / "video")
            shutil.copytree(streams / audio, destination / "audio")
            playlist = destination / "master.m3u8"
            playlist.write_text('#EXTM3U\n#EXT-X-VERSION:6\n'
                                '#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="default-audio-group",NAME="Main",LANGUAGE="en",DEFAULT=NO,AUTOSELECT=YES,URI="audio/media.m3u8"\n'
                                '#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO="default-audio-group"\nvideo/media.m3u8\n',
                                encoding="utf-8")
        else:
            shutil.copytree(streams / layout, destination, dirs_exist_ok=True)
            playlist = destination / "media.m3u8"
        result[layout] = playlist
    return result
