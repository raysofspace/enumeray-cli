#!/usr/bin/python3
"""Controlled scheduler evidence for runner cancellation regression tests."""
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

if sys.argv[1:] == ["--fixture-ready"]:
    sys.exit(0)

# macOS fixtures share warmed executables and retain per-test state separately.
state_root = os.environ.get("SLURM_CONF")
root = (Path(state_root) if state_root and Path(state_root).parent.name.startswith("enumeray-slurm-")
        else Path(__file__).parent)
command = Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as calls:
    calls.write(json.dumps({"command": command, "args": args,
                           "routing_environment": os.environ.get("SLURM_CLUSTERS")}) + "\n")

def read(name, default=""):
    path = root / name
    return path.read_text() if path.exists() else default

cluster = args[args.index("--clusters") + 1] if "--clusters" in args else "implicit"

state = read("state-" + cluster, read("state"))
if command in ("squeue", "sacct") and "--name" in args:
    assert cluster == "all"
    name = "enumeray-" + (root / "intent").read_text()
    assert args[args.index("--user") + 1] == str(os.getuid())
    clusters = {"cluster-a", "cluster-b"}
    clusters.update(p.name.removeprefix("recovery-") for p in root.glob("recovery-cluster-*"))
    for actual_cluster in sorted(clusters):
        recovery = read("recovery-" + actual_cluster,
                        read("recovery") if actual_cluster == "cluster-a" else "missing")
        if recovery == "unavailable":
            sys.exit(1)
        if recovery == "malformed":
            print("invalid evidence")
        elif recovery != "missing":
            uid = os.getuid() + (1 if recovery == "wrong-user" else 0)
            actual_name = "enumeray-another-intent" if recovery == "wrong-name" else name
            reported_cluster = actual_cluster
            if recovery == "conflicting-clusters" and command == "sacct":
                reported_cluster = "cluster-b"
            if (recovery != "accounting-only" or command == "sacct") and (recovery != "queue-only" or command == "squeue"):
                if command == "squeue":
                    print(f"CLUSTER: {reported_cluster}")
                print(f"{reported_cluster:128}|{123:32}|{actual_name:64}|{uid:32}")
                if recovery == "ambiguous":
                    print(f"{reported_cluster}|456|{actual_name}|{uid}")
elif command == "squeue":
    if read("queue-purged") == "1" and "--jobs" in args:
        sys.exit("slurm_load_jobs error: Invalid job id specified")
    if (root / "other-queue-job").exists():
        print(read("other-queue-job"))
    if state in ("PENDING", "CONFIGURING", "RUNNING", "COMPLETING") or (
        "--states=all" in args and state in (
            "SUSPENDED", "COMPLETED", "CANCELLED", "FAILED", "TIMEOUT",
            "OUT_OF_MEMORY", "NODE_FAIL", "BOOT_FAIL", "DEADLINE", "PREEMPTED"
        )
    ):
        print(f"123|{state}")
elif command == "sacct":
    if state != "LAG":
        date = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
        name = "enumeray-" + read("intent")
        started = read("started-at", "Unknown")
        end = read("ended-at", date if state not in ("PENDING", "RUNNING", "COMPLETING") else "Unknown")
        print(read("accounting", f"123|{name}|{os.getuid()}|{state}|{read('exit-code', '0:0')}|{started}|{end}"))
        if (root / "queue-after-accounting").exists():
            (root / "state").write_text(read("queue-after-accounting"))
        if (root / "publish-completion").exists():
            destination = json.loads(read("publish-completion"))
            os.link(root / "retained-completion.json", destination)
        if (root / "publish-startup-failure").exists():
            destination = json.loads(read("publish-startup-failure"))
            os.link(root / "retained-startup-failure.json", destination)
elif command == "scancel":
    assert "--state=PENDING" in args or "--state" not in args
    assert cluster == read("expected-cancel-cluster", "cluster-a")
    mode = (root / "mode").read_text()
    if mode == "start-race":
        (root / "state").write_text("RUNNING")
    elif mode == "lost-reply":
        (root / "state").write_text("LAG")
        (root / "mode").write_text("keep-pending")
        sys.exit(1)
elif command == "sbatch":
    # Inspect on-disk intent from inside the submission process: this proves the
    # durable launch identity existed before the backend call.
    intent = json.loads((Path(args[-1]) / "launch-intent.json").read_text())
    assert "--clusters" not in args
    assert "clusters" not in intent
    assert intent["execution_uid"] == str(os.getuid())
    assert intent["script_digest"]
    assert intent["prepared_at"] > 0
    (root / "intent").write_text(intent["intent"])
    assert not os.environ.get("SBATCH_CLUSTERS")
    if read("mode") == "block-submit":
        (root / "submit-blocked").touch()
        deadline = time.monotonic() + 5
        while not (root / "release-submit").exists():
            if time.monotonic() > deadline:
                sys.exit("fixture submission was not released")
            time.sleep(0.01)
    if read("mode") == "reject-submit":
        sys.exit("sbatch: error: Batch job submission failed: Invalid partition name specified")
    if read("mode") == "ambiguous-submit":
        sys.exit("sbatch: error: Batch job submission failed: Socket timed out on send/recv operation")
    if read("mode") == "truncated-rejection":
        sys.stderr.write("sbatch: error: Batch job submission failed: Invalid partition name specified\n" + "x" * 70000)
        sys.exit(1)
    if read("mode") == "conflicting-submit":
        print(read("reply"))
        sys.exit("sbatch: error: Batch job submission failed: Invalid partition name specified")
    if read("mode") == "lost-submit-reply":
        sys.exit(1)
    print(read("reply"))
else:
    sys.exit("Unexpected scheduler command")
