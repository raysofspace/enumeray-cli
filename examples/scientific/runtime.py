"""Bounded local execution shared by the two reviewed scientific demonstrations."""
from decimal import Decimal
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_OUTPUT_BYTES = 8 * 1024 * 1024
LIMITS = {"huxt": 180, "rebound": 60}
PARAMETERS = {
    "huxt": {"slow_speed_kms": (250, 450), "fast_speed_kms": (500, 900),
             "stream_width_deg": (10, 45)},
    "rebound": {"semimajor_axis_au": (0.1, 5), "eccentricity": (0, 0.8),
                "orbits": (0.25, 10)},
}
FILES = {"huxt": {"wind_field_csv": ("wind-field.csv", "text/csv"),
                  "wind_plot_png": ("wind.png", "image/png")},
         "rebound": {"trajectory_csv": ("trajectory.csv", "text/csv"),
                     "orbit_plot_png": ("orbit.png", "image/png")}}


class ExecutionFailure(Exception):
    def __init__(self, code):
        self.code = code
        super().__init__(code)


def validate_inputs(model, inputs):
    expected = PARAMETERS[model]
    if not isinstance(inputs, dict) or inputs.keys() != expected.keys():
        raise ExecutionFailure("invalid_inputs")
    values = {}
    for name, (low, high) in expected.items():
        value = inputs[name]
        if isinstance(value, bool) or not isinstance(value, (int, float, Decimal)):
            raise ExecutionFailure("invalid_inputs")
        if not math.isfinite(value) or not low <= value <= high:
            raise ExecutionFailure("invalid_inputs")
        values[name] = float(value)
    return values


def worker_environment(workspace):
    private = Path(workspace) / ".scientific"
    private.mkdir(mode=0o700)
    result = {"PATH": os.path.dirname(sys.executable) + ":/usr/bin:/bin", "LANG": "C.UTF-8",
              "PYTHONDONTWRITEBYTECODE": "1", "PYTHONNOUSERSITE": "1", "MPLBACKEND": "Agg",
              "OPENBLAS_NUM_THREADS": "1", "OMP_NUM_THREADS": "1", "MKL_NUM_THREADS": "1",
              "NUMBA_NUM_THREADS": "1", "NUMEXPR_NUM_THREADS": "1", "VECLIB_MAXIMUM_THREADS": "1"}
    for name, directory in (("HOME", "home"), ("XDG_DATA_HOME", "data"),
                            ("XDG_CACHE_HOME", "cache"), ("MPLCONFIGDIR", "matplotlib"),
                            ("NUMBA_CACHE_DIR", "numba"), ("TMPDIR", "tmp")):
        target = private / directory
        target.mkdir(mode=0o700)
        result[name] = str(target)
    return result


def supervise(command, *, cwd, env, timeout):
    """Keep the solver in the runner's process group; reap it on timeout/cancel."""
    child = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.DEVNULL)
    cancelled = []
    previous = {}

    def cancel(signum, _frame):
        cancelled.append(signum)
        if child.poll() is None:
            child.terminate()

    for signum in (signal.SIGTERM, signal.SIGINT):
        previous[signum] = signal.signal(signum, cancel)
    deadline = time.monotonic() + timeout
    try:
        while child.poll() is None:
            if cancelled or time.monotonic() >= deadline:
                child.terminate()
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                raise ExecutionFailure("cancelled" if cancelled else "runtime_timeout")
            time.sleep(0.02)
        if cancelled:
            raise ExecutionFailure("cancelled")
        if child.returncode:
            raise ExecutionFailure("model_failure")
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def run_model(model, inputs, workspace):
    values = validate_inputs(model, inputs)
    workspace = Path(workspace).resolve()
    environment = worker_environment(workspace)
    private = workspace / ".scientific"
    source = private / "inputs.json"
    source.write_text(json.dumps(values, allow_nan=False))
    result = private / "outputs.json"
    supervise([sys.executable, "-B", str(ROOT / "worker.py"), model, str(source),
               str(result), str(workspace), str(os.getpid())], cwd=workspace,
              env=environment, timeout=LIMITS[model])
    if result.is_symlink() or not result.is_file() or result.stat().st_size > 65536:
        raise ExecutionFailure("invalid_outputs")
    outputs = json.loads(result.read_text(), parse_constant=lambda _: None)
    if not isinstance(outputs, dict) or any(isinstance(value, bool) or not isinstance(value, (int, float))
                                          or not math.isfinite(value) for value in outputs.values()):
        raise ExecutionFailure("invalid_outputs")
    files = {}
    total_bytes = 0
    for slot, (filename, media_type) in FILES[model].items():
        path = workspace / filename
        if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= MAX_FILE_BYTES:
            raise ExecutionFailure("invalid_outputs")
        if media_type == "image/png" and path.read_bytes()[:8] != b"\x89PNG\r\n\x1a\n":
            raise ExecutionFailure("invalid_outputs")
        total_bytes += path.stat().st_size
        files[slot] = {"path": filename, "media_type": media_type, "scientific_metadata": {}}
    if total_bytes > MAX_OUTPUT_BYTES:
        raise ExecutionFailure("output_limit")
    return outputs, files
