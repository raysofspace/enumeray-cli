"""Edit fixed argv and native input/output mapping for your existing executable.

The demo uses Python, but the solver may be any installed foreground executable.
Do not derive the executable, flags, hosts, or credentials from scientific input.
"""
from decimal import Decimal
from pathlib import Path
import subprocess
import sys
import tempfile
from protocol import MAX_FILE_BYTES, ModelError, exact_json, publish, read_json, staged_dataset

RELEASE = Path(__file__).resolve().parent
# Replace with a fixed executable from your immutable installed release.
SOLVER_ARGV = [sys.executable, str(RELEASE / "demo_solver.py")]


def evaluate(request, workspace, mode):
    if mode == "json":
        if request["input_files"] or request["inputs"].keys() != {"value"} or not isinstance(request["inputs"]["value"], Decimal):
            raise ModelError("One finite numeric value is required.")
        source = None
    else:
        source = staged_dataset(request, workspace)
    # Native scratch files never replace retained wrapper or scientific outputs.
    with tempfile.TemporaryDirectory(prefix=".native-", dir=workspace) as temporary:
        root = Path(temporary)
        if source is None:
            source = root / "inputs.json"
            publish(source, (exact_json(request["inputs"]) + "\n").encode())
        target = root / "output"
        completed = subprocess.run(
            [*SOLVER_ARGV, "--mode", mode, "--input", str(source), "--output", str(target)],
            shell=False, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, cwd=workspace, check=False,
        )
        if completed.returncode:
            raise ModelError("Scientific executable failed; inspect its declared native interface.")
        if mode == "json":
            value = read_json(target)
            if not isinstance(value, dict) or value.keys() != {"doubled"} or not isinstance(value["doubled"], Decimal):
                raise ModelError("Invalid native scientific result.")
            return value, {}
        with target.open("rb") as result:
            content = result.read(MAX_FILE_BYTES + 1)
        count = len(content.splitlines()) - 1
        publish(workspace / "processed.csv", content, MAX_FILE_BYTES)
        return {"sample_count": count}, {"processed_dataset": {
            "path": "processed.csv", "media_type": "text/csv", "scientific_metadata": {}}}
