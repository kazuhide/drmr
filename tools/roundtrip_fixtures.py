"""Generate three small source streams once and reuse qualified profile graphs."""
import hashlib
import os
import re
import shutil


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
            text[media[-1] - 1] = f"#EXTINF:{240 / rate:.17f},"
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




def cenc_hls(dash, clear_hls, destination, uri, support, inspect_kind):
    """Combine verified declarations and CENC bytes by actual track kind.

    Only playlist paths and the CENC key declaration change. Endpoint strings
    come from the independently verified clear HLS companion. Fresh native
    comparison of the new HLS graph remains mandatory; matching filenames or
    declared segment durations alone never qualify the generated fixture.
    """
    shutil.copytree(dash.parent, destination)
    initializations = {}
    for initialization in destination.rglob("*.mp4"):
        kind = inspect_kind(initialization)
        support.require(kind not in initializations, "fixture-track-ambiguous")
        initializations[kind] = initialization
    support.require(1 <= len(initializations) <= 2, "fixture-track-invalid")
    playlists = list(clear_hls.parent.rglob("*.m3u8"))
    support.require(1 <= len(playlists) <= 4, "fixture-playlists-invalid")
    replacements, media = {}, []
    for playlist in playlists:
        lines = playlist.read_text(encoding="utf-8").splitlines()
        if not any(line.startswith("#EXTINF:") for line in lines):
            continue
        support.require(not any(line.startswith("#EXT-X-KEY:") for line in lines), "fixture-clear-key-invalid")
        maps = [index for index, line in enumerate(lines) if line.startswith("#EXT-X-MAP:")]
        support.require(len(maps) == 1, "fixture-map-invalid")
        matched = re.search(r'URI="([^"\r\n]+)"', lines[maps[0]])
        support.require(matched is not None, "fixture-map-invalid")
        clear_init = (playlist.parent / matched.group(1)).resolve()
        support.require(clear_init.is_relative_to(clear_hls.parent.resolve()) and clear_init.is_file(), "fixture-map-invalid")
        kind = inspect_kind(clear_init)
        support.require(kind in initializations, "fixture-track-invalid")
        initialization = initializations[kind]
        output = initialization.parent / playlist.name
        support.require(output not in replacements.values(), "fixture-playlists-ambiguous")
        replacements[playlist.resolve()] = output
        lines[maps[0]] = f'#EXT-X-MAP:URI="{initialization.name}"'
        for index, line in enumerate(lines):
            if not line or line.startswith("#"):
                continue
            name = line.rsplit("/", 1)[-1]
            resource = output.parent / name
            support.require(resource.is_file() and not resource.is_symlink(), "fixture-resource-invalid")
            lines[index] = name
        lines.insert(maps[0], f'#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI="{uri}"')
        media.append((output, lines))
    support.require(len(media) == len(initializations), "fixture-track-invalid")
    for output, lines in media:
        output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    for playlist in playlists:
        if playlist.resolve() in replacements:
            continue
        output = destination / playlist.relative_to(clear_hls.parent)
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        lines = playlist.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if not line or line.startswith("#") and not line.startswith("#EXT-X-MEDIA:"):
                continue
            if line.startswith("#EXT-X-MEDIA:"):
                match = re.search(r'URI="([^"\r\n]+)"', line)
                support.require(match is not None, "fixture-playlist-reference-invalid")
                name = match.group(1)
            else:
                name = line
            original = (playlist.parent / name).resolve()
            support.require(original in replacements, "fixture-playlist-reference-invalid")
            relative = os.path.relpath(replacements[original], output.parent).replace(os.sep, "/")
            lines[index] = line.replace(f'URI="{name}"', f'URI="{relative}"') if line.startswith("#") else relative
        output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    (destination / "manifest.mpd").unlink()
    return destination / clear_hls.name
