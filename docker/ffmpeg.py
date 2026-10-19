"""Build the pinned ffmpeg component without runtime network installation."""
from native import PREFIX, WORK, RECIPE, collect_notices, digest, fetch_archive, git_archive, record, run, unpack, verify_file
import json
import urllib.request
import shutil
def ffmpeg(pin, jobs):
    source = unpack(fetch_archive(pin), WORK / "ffmpeg")
    signature = PREFIX / "sources" / pin["signature"]["archive"]
    with urllib.request.urlopen(pin["signature"]["url"], timeout=60) as response:
        signature.write_bytes(response.read(65536))
    verify_file(signature, pin["signature"]["sha256"])
    key = RECIPE / "ffmpeg-release.asc"
    verify_file(key, pin["key_sha256"])
    keyring = WORK / "ffmpeg-keyring.gpg"
    run("gpg", "--batch", "--dearmor", "--output", str(keyring), str(key))
    run("gpgv", "--keyring", str(keyring), str(signature), str(PREFIX / "sources" / pin["archive"]))
    flags = ["--prefix=/opt/drmr", "--disable-autodetect", "--disable-shared", "--enable-static",
             "--disable-ffplay", "--disable-debug", "--disable-doc"]
    run("./configure", *flags, cwd=source)
    run("make", "-j" + str(jobs), cwd=source)
    run("make", "install", cwd=source)
    record("ffmpeg", pin, source, {"configure": flags, "license": "LGPL-2.1-or-later; file-specific notices retained"})


if __name__ == "__main__":
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description="Build the pinned native component")
    parser.add_argument("--jobs", type=int, default=2, choices=range(1, 9))
    args = parser.parse_args()
    (PREFIX / "bin").mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    pins = json.loads((Path(__file__).parent / "sources.json").read_text())
    ffmpeg(pins["ffmpeg"], args.jobs)
