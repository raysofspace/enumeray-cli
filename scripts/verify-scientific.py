#!/usr/bin/env python3
"""Run bounded real scientific qualification with a prepared locked interpreter."""
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
python = os.environ.get("ENUMERAY_SCIENTIFIC_PYTHON", sys.executable)
for arguments in (("-m", "unittest", "discover", "-s", "examples/scientific", "-p", "test_models.py"),
                  ("examples/scientific/qualify.py", "--extrema")):
    subprocess.run([python, "-B", *arguments], cwd=root, check=True)
