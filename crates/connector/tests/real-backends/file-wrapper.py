#!/usr/bin/python3
import json
import math
import os
import sys
import time
from pathlib import Path

def bounded_integer(environ, name, default, maximum, minimum=0):
    raw = environ.get(name, str(default))
    if not isinstance(raw, str) or not raw.isascii() or not raw.isdigit():
        raise ValueError(name + " must be a bounded non-negative integer")
    value = int(raw)
    if not minimum <= value <= maximum:
        raise ValueError(name + " exceeds the fixture workload bounds")
    return value


def workload(request, environ):
    """Keep large capture/cleanup coverage explicit instead of multiplying it."""
    inputs = request.get("inputs", {})
    if not isinstance(inputs, dict):
        raise ValueError("Fixture inputs must be an object")
    profile = inputs.get("fixture_profile", "coverage")
    if profile not in ("coverage", "coverage-controlled", "functional", "controlled"):
        raise ValueError("Unknown fixture workload profile")
    full = profile in ("coverage", "coverage-controlled")
    log_bytes = bounded_integer(environ, "ENUMERAY_ACCEPTANCE_LOG_BYTES",
                                2 * 1024 * 1024 if full else 64, 8 * 1024 * 1024)
    scratch_bytes = bounded_integer(environ, "ENUMERAY_ACCEPTANCE_SCRATCH_BYTES",
                                    4 * 1024 * 1024 if full else 0, 64 * 1024 * 1024)
    scratch_entries = bounded_integer(environ, "ENUMERAY_ACCEPTANCE_SCRATCH_ENTRIES",
                                      600 if full else 0, 4096)
    delay = float(environ.get("ENUMERAY_ACCEPTANCE_DELAY", "0"))
    if not math.isfinite(delay) or not 0 <= delay <= 180:
        raise ValueError("ENUMERAY_ACCEPTANCE_DELAY exceeds the fixture workload bounds")
    gate_seconds = bounded_integer(environ, "ENUMERAY_ACCEPTANCE_GATE_SECONDS", 180, 600, minimum=1)
    return {"profile": profile, "log_bytes": log_bytes,
            "scratch_bytes": scratch_bytes, "scratch_entries": scratch_entries,
            "delay": delay if profile == "coverage" else 0, "gate_seconds": gate_seconds}


def execution_environment():
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


def wait_for_control(settings):
    # The harness observes readiness before disconnecting/cancelling. Normal
    # completion requires an explicit continue marker, never a timing guess.
    Path(".acceptance-ready").write_text("ready\n")
    deadline = time.monotonic() + settings["gate_seconds"]
    while not Path(".acceptance-continue").exists():
        if time.monotonic() >= deadline:
            raise RuntimeError("controlled fixture gate deadline exceeded")
        time.sleep(0.025)


def main():
    execution_environment()
    request = json.loads(Path("request.json").read_text())
    settings = workload(request, os.environ)
    # Named diagnostics retain invalid UTF-8 even for small functional trials.
    Path("solver.log").write_bytes(b"solver converged\x00\xff\n")
    sys.stdout.buffer.write(b"o" * (settings["log_bytes"] + 17))
    sys.stdout.buffer.flush()
    sys.stderr.buffer.write(b"e" * (settings["log_bytes"] + 31))
    sys.stderr.buffer.flush()
    if os.environ.get("ENUMERAY_ACCEPTANCE_SCRATCH"):
        if settings["scratch_bytes"] or settings["scratch_entries"]:
            Path("scratch-heavy").mkdir()
            Path("scratch-heavy/intermediate").write_bytes(b"x" * settings["scratch_bytes"])
            for i in range(settings["scratch_entries"]):
                Path("scratch-heavy", str(i)).touch()
            Path("scratch-heavy/" + "/".join(["deep"] * 12)).mkdir(parents=True)
            Path("scratch-alias").symlink_to("scratch-heavy")
        if not os.environ.get("ENUMERAY_ACCEPTANCE_EXPECT_SLURM_SETUP"):
            assert Path(os.environ["TMPDIR"]).resolve() == Path(".enumeray/tmp").resolve()
            assert Path(os.environ["XDG_CACHE_HOME"]).resolve() == Path(".enumeray/cache").resolve()
            Path(os.environ["XDG_CACHE_HOME"], "cache-data").write_bytes(b"cache")
    if settings["profile"] in ("controlled", "coverage-controlled"):
        wait_for_control(settings)
    elif settings["delay"]:
        time.sleep(settings["delay"])
    output = "../escape.txt" if os.environ.get("ENUMERAY_ACCEPTANCE_OUTPUT_ESCAPE") else "answer.txt"
    if os.environ.get("ENUMERAY_ACCEPTANCE_LARGE_FILES"):
        # A second scientific invocation under the same launch directory fails.
        with Path("science-launched").open("x") as marker:
            marker.write("one\n")
        with Path(request["input_files"]["dataset"]["path"]).open("rb") as source, Path(output).open("wb") as target:
            while chunk := source.read(1024 * 1024):
                target.write(chunk)
            target.flush()
            os.fsync(target.fileno())
    else:
        value = int(Path(request["input_files"]["dataset"]["path"]).read_text())
        Path(output).write_text(str(value * 2))
    for key in ["inputs", "model_id", "publication_id", "input_files"]:
        request.pop(key, None)
    request.update(outcome="succeeded", outputs={}, failure=None, files={
        "result": {"path": output, "media_type": "text/plain", "scientific_metadata": {}}
    })
    Path("result.json").write_text(json.dumps(request))


if __name__ == "__main__":
    main()
