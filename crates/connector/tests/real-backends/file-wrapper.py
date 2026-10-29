#!/usr/bin/python3
import json
import os
import sys
import time
from pathlib import Path

assert os.getuid() == 10002
try:
    Path("/home/enumeray/.ssh/id_ed25519").read_bytes()
except PermissionError:
    pass
else:
    raise RuntimeError("model could read connector credentials")
assert not any(key.startswith("ENUMERAY_S3_") for key in os.environ)
assert Path(os.environ["HOME"]).resolve() == Path.cwd()
assert os.environ["PYTHONDONTWRITEBYTECODE"] == "1"
if os.environ.get("ENUMERAY_ACCEPTANCE_EXPECT_SLURM_SETUP"):
    assert os.environ.get("SLURM_JOB_ID")
    assert os.environ.get("SLURM_JOB_NODELIST")
    assert os.environ.get("ENUMERAY_SLURM_SETUP") == "ready"
    assert os.environ.get("MODULEPATH") == "/opt/enumeray/batch-modules"
    assert os.environ.get("LD_LIBRARY_PATH") == "/opt/enumeray/batch-libs"
    assert os.environ.get("PYTHONPATH") == "/opt/enumeray/batch-python"
    assert os.environ.get("OMP_NUM_THREADS") == "3"
# Named diagnostics are workspace files; raw archive bytes include invalid UTF-8.
Path("solver.log").write_bytes(b"solver converged\x00\xff\n")
# More output than both the capture and scratch budgets must still drain.
sys.stdout.buffer.write(b"o" * (2 * 1024 * 1024 + 17))
sys.stdout.buffer.flush()
sys.stderr.buffer.write(b"e" * (2 * 1024 * 1024 + 31))
sys.stderr.buffer.flush()
if os.environ.get("ENUMERAY_ACCEPTANCE_SCRATCH"):
    Path("scratch-heavy").mkdir()
    Path("scratch-heavy/intermediate").write_bytes(b"x" * (4 * 1024 * 1024))
    for i in range(600):
        Path("scratch-heavy", str(i)).touch()
    Path("scratch-heavy/" + "/".join(["deep"] * 12)).mkdir(parents=True)
    Path("scratch-alias").symlink_to("scratch-heavy")
    if not os.environ.get("ENUMERAY_ACCEPTANCE_EXPECT_SLURM_SETUP"):
        assert Path(os.environ["TMPDIR"]).resolve() == Path(".enumeray/tmp").resolve()
        assert Path(os.environ["XDG_CACHE_HOME"]).resolve() == Path(".enumeray/cache").resolve()
        Path(os.environ["XDG_CACHE_HOME"], "cache-data").write_bytes(b"cache")
time.sleep(float(os.environ.get("ENUMERAY_ACCEPTANCE_DELAY", "0")))
r = json.loads(Path("request.json").read_text())
output = "../escape.txt" if os.environ.get("ENUMERAY_ACCEPTANCE_OUTPUT_ESCAPE") else "answer.txt"
if os.environ.get("ENUMERAY_ACCEPTANCE_LARGE_FILES"):
    # A second scientific invocation under the same launch directory must fail.
    with Path("science-launched").open("x") as marker:
        marker.write("one\n")
    with Path(r["input_files"]["dataset"]["path"]).open("rb") as source, Path(output).open("wb") as target:
        while chunk := source.read(1024 * 1024):
            target.write(chunk)
        target.flush()
        os.fsync(target.fileno())
else:
    value = int(Path(r["input_files"]["dataset"]["path"]).read_text())
    Path(output).write_text(str(value * 2))
for key in ["inputs", "model_id", "publication_id", "input_files"]:
    r.pop(key, None)
r.update(outcome="succeeded", outputs={}, failure=None, files={
    "result": {"path": output, "media_type": "text/plain", "scientific_metadata": {}}
})
Path("result.json").write_text(json.dumps(r))
