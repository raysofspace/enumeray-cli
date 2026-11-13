"""Private solver child. No service credentials or runtime network are needed."""
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import resource
import signal
import socket
import sys

from runtime import FILES


def isolate(parent):
    if sys.platform.startswith("linux"):
        # A killed supervisor must not leave the native scientific child running.
        if ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGKILL, 0, 0, 0):
            raise RuntimeError("Cannot bind solver lifetime to its supervisor")
    if os.getppid() != parent:
        raise RuntimeError("Scientific supervisor has exited")
    resource.setrlimit(resource.RLIMIT_FSIZE, (8 * 1024 * 1024, 8 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_CPU, (180, 180))

    def no_network(*_args, **_kwargs):
        raise RuntimeError("Scientific demonstration uses bundled offline inputs only")
    socket.create_connection = no_network
    socket.getaddrinfo = no_network
    socket.socket.connect = no_network
    socket.socket.connect_ex = no_network


def main():
    model, inputs, output, workspace, parent = sys.argv[1:]
    if model not in FILES:
        return 2
    isolate(int(parent))
    root = Path(__file__).resolve().parent
    spec = importlib.util.spec_from_file_location("scientific_model", root / model / "model.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    values = module.simulate(json.loads(Path(inputs).read_text()), Path(workspace))
    # Link publication preserves any prior worker result on uncertainty.
    from protocol import publish
    publish(output, (json.dumps(values, allow_nan=False) + "\n").encode())
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if model == "lammps":
        # The serial LAMMPS executable runs while the Python worker is resident.
        peak += resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    metrics = {"peak_rss_bytes": int(peak if sys.platform == "darwin" else peak * 1024),
               "offline": True}
    publish(Path(output).with_name("metrics.json"), (json.dumps(metrics) + "\n").encode())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
