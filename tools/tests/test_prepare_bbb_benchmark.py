"""Check provenance, destination boundaries and actual generated media declarations."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPT = Path(__file__).resolve().parents[1] / "prepare_bbb_benchmark.py"
FIXTURE_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"


def invoke(script, output):
    result = subprocess.run([sys.executable, str(script), "--output", str(output)],
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=240, check=False)
    return result, json.loads(result.stdout)


class BenchmarkInput(unittest.TestCase):
    def test_existing_output_and_descriptor_separators_cannot_redirect_generation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sentinel = root / "sentinel"
            sentinel.write_bytes(b"keep this file")
            result, report = invoke(SCRIPT, root)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["error"], "output-already-exists")
            self.assertEqual(list(root.iterdir()), [sentinel])
            self.assertEqual(sentinel.read_bytes(), b"keep this file")
            destination = root / "source,stream=audio"
            result, report = invoke(SCRIPT, destination)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["error"], "output-path-not-supported")
            self.assertFalse(destination.exists())

    def test_modified_fixture_is_refused_before_output_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "tools" / SCRIPT.name
            script.parent.mkdir()
            shutil.copyfile(SCRIPT, script)
            fixture = root / "crates/drmr/tests/fixtures/bbb/clip.mp4"
            fixture.parent.mkdir(parents=True)
            fixture.write_bytes(b"not the attributed clip")
            destination = root / "generated"
            result, report = invoke(script, destination)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["error"], "fixture-identity-mismatch")
            self.assertFalse(destination.exists())

    def test_real_tools_generate_eight_videos_with_five_two_second_segments(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "input with spaces"
            result, report = invoke(SCRIPT, destination)
            self.assertEqual(result.returncode, 0, report)
            self.assertEqual(result.stderr, b"")
            self.assertTrue(report["success"])
            self.assertIn("creativecommons.org/licenses/by/3.0/",
                          (destination / "ATTRIBUTION.md").read_text())
            self.assertEqual(hashlib.sha256((destination / "source-clip.mp4").read_bytes()).hexdigest(),
                             FIXTURE_SHA256)
            details = json.loads((destination / "input-report.json").read_text())
            self.assertEqual(details["fixture_sha256"], FIXTURE_SHA256)
            self.assertEqual(details["video_renditions"], 8)
            self.assertEqual(details["audio_tracks"], 1)
            for resource in details["resources"]:
                content = (destination / "source" / resource["resource"]).read_bytes()
                self.assertEqual(len(content), resource["bytes"])
                self.assertEqual(hashlib.sha256(content).hexdigest(), resource["sha256"])
            namespace = {"d": "urn:mpeg:dash:schema:mpd:2011"}
            manifest = ET.parse(destination / "source/manifest.mpd")
            counts = {"video": 0, "audio": 0}
            for adaptation in manifest.findall(".//d:AdaptationSet", namespace):
                kind = adaptation.attrib["contentType"]
                for representation in adaptation.findall("d:Representation", namespace):
                    counts[kind] += 1
                    if kind != "video":
                        continue
                    width = representation.attrib.get("width", adaptation.attrib.get("width"))
                    height = representation.attrib.get("height", adaptation.attrib.get("height"))
                    self.assertEqual(int(width), 1280)
                    self.assertEqual(int(height), 720)
                    template = representation.find("d:SegmentTemplate", namespace)
                    scale = int(template.attrib["timescale"])
                    timeline = template.findall("d:SegmentTimeline/d:S", namespace)
                    self.assertTrue(timeline)
                    self.assertEqual(sum(int(entry.attrib.get("r", "0")) + 1 for entry in timeline), 5)
                    self.assertTrue(all(int(entry.attrib["d"]) == 2 * scale for entry in timeline))
            self.assertEqual(counts, {"video": 8, "audio": 1})


if __name__ == "__main__":
    unittest.main()
