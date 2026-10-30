"""Generate three small source streams once and reuse qualified profile graphs."""
import hashlib
from pathlib import Path
import re
import shutil
import xml.etree.ElementTree as ET


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


def substitute(template, representation, number, timestamp):
    values = {"RepresentationID": representation.get("id"), "Bandwidth": representation.get("bandwidth"),
              "Number": number, "Time": timestamp}

    def replace(match):
        value = values[match.group(1)]
        if value is None:
            raise ValueError("fixture-template-invalid")
        width = int(match.group(2) or 0)
        return str(value).zfill(width)

    return re.sub(r"\$(RepresentationID|Bandwidth|Number|Time)(?:%0([1-9][0-9]?)d)?\$", replace, template)


def seconds(ticks, timescale):
    """Use the same integer nanosecond ceiling as the native manifest writer."""
    nanoseconds = (ticks * 1_000_000_000 + timescale - 1) // timescale
    return f"{nanoseconds // 1_000_000_000}.{nanoseconds % 1_000_000_000:09d}".rstrip("0").rstrip(".")


def cenc_hls(dash, destination, uri, support):
    """Change only declarations around already-verified CENC sample bytes.

    This input-only profile deliberately cannot be produced by drmr. Its init
    boxes, edit lists, fragments and sample clocks stay byte-identical to the
    cached CENC DASH fixture; fresh native verification binds the new HLS graph.
    """
    shutil.copytree(dash.parent, destination)
    mpd = ET.parse(destination / "manifest.mpd").getroot()
    namespace = {"d": "urn:mpeg:dash:schema:mpd:2011"}
    references = []
    for adaptation in mpd.findall(".//d:AdaptationSet", namespace):
        for representation in adaptation.findall("d:Representation", namespace):
            template = representation.find("d:SegmentTemplate", namespace)
            if template is None:
                template = adaptation.find("d:SegmentTemplate", namespace)
            support.require(template is not None, "fixture-template-invalid")
            timeline = template.find("d:SegmentTimeline", namespace)
            support.require(timeline is not None, "fixture-timeline-required")
            timescale = int(template.get("timescale", "1"))
            number, timestamp, segments = int(template.get("startNumber", "1")), 0, []
            for item in timeline.findall("d:S", namespace):
                timestamp = int(item.get("t", timestamp))
                duration, repeat = int(item.get("d", "0")), int(item.get("r", "0"))
                support.require(timescale > 0 and duration > 0 and 0 <= repeat <= 16, "fixture-timeline-invalid")
                for _ in range(repeat + 1):
                    name = substitute(template.get("media", ""), representation, number, timestamp)
                    support.require((destination / name).is_file() and len(segments) < 16, "fixture-resource-invalid")
                    segments.append((duration, name))
                    timestamp += duration
                    number += 1
            init = substitute(template.get("initialization", ""), representation, number, timestamp)
            support.require((destination / init).is_file() and bool(segments), "fixture-resource-invalid")
            kind = adaptation.get("contentType") or representation.get("mimeType", adaptation.get("mimeType", "")).split("/")[0]
            support.require(kind in ("audio", "video"), "fixture-track-invalid")
            name = f"{kind}.m3u8"
            lines = ["#EXTM3U", "#EXT-X-VERSION:7", "#EXT-X-PLAYLIST-TYPE:VOD", "#EXT-X-MEDIA-SEQUENCE:1",
                     f"#EXT-X-TARGETDURATION:{(max(duration for duration, _ in segments) + timescale - 1) // timescale}",
                     f'#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI="{uri}"', f'#EXT-X-MAP:URI="{init}"']
            for duration, media in segments:
                lines += [f"#EXTINF:{seconds(duration, timescale)},", media]
            (destination / name).write_text("\n".join(lines + ["#EXT-X-ENDLIST", ""]), encoding="utf-8")
            references.append((kind, name))
    support.require(1 <= len(references) <= 2, "fixture-track-invalid")
    master = ["#EXTM3U", "#EXT-X-VERSION:7"]
    audio = next((name for kind, name in references if kind == "audio"), None)
    video = next((name for kind, name in references if kind == "video"), None)
    if audio and video:
        master += [f'#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="default-audio-group",NAME="Main",LANGUAGE="en",DEFAULT=NO,AUTOSELECT=YES,URI="{audio}"',
                   '#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO="default-audio-group"', video]
    else:
        master += ["#EXT-X-STREAM-INF:BANDWIDTH=1000000", video or audio]
    (destination / "master.m3u8").write_text("\n".join(master) + "\n", encoding="utf-8")
    (destination / "manifest.mpd").unlink()
    return destination / "master.m3u8"
