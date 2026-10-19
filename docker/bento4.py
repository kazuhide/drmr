"""Build the pinned bento4 component without runtime network installation."""
from native import PREFIX, WORK, RECIPE, collect_notices, digest, fetch_archive, git_archive, record, run, unpack, verify_file
import json
import shutil
def bento(pin, jobs):
    source = unpack(fetch_archive(pin), WORK / "bento4")
    build = WORK / "bento4-build"
    run("cmake", "-S", str(source), "-B", str(build), "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release")
    run("cmake", "--build", str(build), "--target", "mp4dump", "--parallel", str(jobs))
    shutil.copy2(build / "mp4dump", PREFIX / "bin/mp4dump")
    record("bento4", pin, source, {"license": "GPL-2.0-or-later; no commercial license is bundled"})


if __name__ == "__main__":
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description="Build the pinned native component")
    parser.add_argument("--jobs", type=int, default=2, choices=range(1, 9))
    args = parser.parse_args()
    (PREFIX / "bin").mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    pins = json.loads((Path(__file__).parent / "sources.json").read_text())
    bento(pins["bento4"], args.jobs)
