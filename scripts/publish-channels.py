#!/usr/bin/env python3
"""Submit or inspect immutable channel-update PRs; dry-run unless --submit is set.

Use a GH_TOKEN scoped to the project Homebrew tap. This helper does not publish release assets,
merge PRs, fork repositories, or create repositories.
"""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
from urllib.parse import quote, urlencode


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("generate_channels", Path(__file__).with_name("generate-channels.py"))
generation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generation)


class DeliveryError(Exception):
    pass


class GitHub:
    def request(self, method, endpoint, payload=None, optional=False):
        command = ["gh", "api", "--method", method, endpoint]
        if payload is not None:
            command += ["--input", "-"]
        result = subprocess.run(command, input=json.dumps(payload) if payload is not None else None,
                                text=True, capture_output=True, check=False)
        if result.returncode:
            if optional and "HTTP 404" in result.stderr:
                return None
            # Do not copy arbitrary provider replies or credentials into reports.
            raise DeliveryError("GitHub request failed: " + method + " " + endpoint.split("?", 1)[0])
        try:
            return json.loads(result.stdout)
        except ValueError:
            raise DeliveryError("GitHub returned invalid JSON") from None


def load_delivery(directory, channel, config):
    generation.validate_config(config)
    directory = Path(directory).resolve()
    plan = json.loads((directory / "channel-plan.json").read_text())
    if plan.get("schema_version") != 1 or channel not in plan.get("channels", {}):
        raise DeliveryError("The requested channel is absent from this delivery plan")
    entry = plan["channels"][channel]
    settings = config.get(channel, {})
    for name in ("repository", "base_branch"):
        if entry.get(name) != settings.get(name, "main" if name == "base_branch" else None):
            raise DeliveryError("Channel configuration changed; regenerate into a new delivery directory")
    version = plan.get("release", {}).get("version", "")
    try:
        generation.artifacts_policy.validate_version(version)
        generation.artifacts_policy.validate_baseline(plan.get("release", {}).get("baseline"), version)
    except ValueError as error:
        raise DeliveryError(str(error)) from None
    if entry.get("branch") != "codex/enumeray-v" + version:
        raise DeliveryError("The update branch does not identify this immutable version")
    desired = {}
    for item in entry.get("files", []):
        source = (directory / item["path"]).resolve()
        destination = PurePosixPath(item["destination"])
        if (not source.is_relative_to(directory) or destination.is_absolute()
                or ".." in destination.parts or not destination.parts
                or str(destination) in desired):
            raise DeliveryError("Delivery path escapes its channel")
        contents = source.read_bytes()
        if hashlib.sha256(contents).hexdigest() != item["sha256"]:
            raise DeliveryError("Generated channel content changed after planning")
        desired[str(destination)] = contents.decode("utf-8")
    return plan, entry, desired


def find_pull_request(api, repository, head_repository, branch, base):
    query = urlencode({"state": "all", "head": head_repository.split("/")[0] + ":" + branch,
                       "base": base, "per_page": 100})
    matches = api.request("GET", "repos/" + repository + "/pulls?" + query)
    if len(matches) > 1:
        raise DeliveryError("Multiple pull requests already identify this release version")
    return matches[0] if matches else None


def verify_branch_contents(api, repository, revision, desired):
    for path, expected in desired.items():
        item = api.request("GET", "repos/" + repository + "/contents/" + quote(path, safe="/")
                           + "?" + urlencode({"ref": revision}), optional=True)
        try:
            actual = base64.b64decode(item["content"]).decode("utf-8") if item else None
        except (ValueError, KeyError, UnicodeError):
            raise DeliveryError("Invalid channel content returned by GitHub") from None
        if actual != expected:
            raise DeliveryError("Conflicting package content already exists for this version")


def submit_channel(api, plan, entry, desired):
    repository = entry["repository"]
    head_repository = repository
    branch = entry["branch"]
    base = entry["base_branch"]
    pr = find_pull_request(api, repository, head_repository, branch, base)
    if pr:
        verify_branch_contents(api, head_repository, pr["head"]["sha"], desired)
        if pr.get("merged_at"):
            return {"state": "delivered", "url": pr["html_url"]}
        if pr.get("state") == "closed":
            return {"state": "needs-maintainer", "url": pr["html_url"],
                    "reason": "The existing update was closed without merging"}
        return {"state": "pending-review", "url": pr["html_url"]}

    reference = api.request("GET", "repos/" + head_repository + "/git/ref/heads/"
                            + quote(branch, safe="/"), optional=True)
    if reference:
        verify_branch_contents(api, head_repository, reference["object"]["sha"], desired)
    else:
        base_ref = api.request("GET", "repos/" + repository + "/git/ref/heads/" + quote(base, safe="/"))
        base_sha = base_ref["object"]["sha"]
        base_commit = api.request("GET", "repos/" + repository + "/git/commits/" + base_sha)
        tree = api.request("POST", "repos/" + head_repository + "/git/trees", {
            "base_tree": base_commit["tree"]["sha"],
            "tree": [{"path": path, "mode": "100644", "type": "blob", "content": contents}
                     for path, contents in sorted(desired.items())],
        })
        commit = api.request("POST", "repos/" + head_repository + "/git/commits", {
            "message": "Update Enumeray to " + plan["release"]["version"],
            "tree": tree["sha"], "parents": [base_sha],
        })
        try:
            api.request("POST", "repos/" + head_repository + "/git/refs", {
                "ref": "refs/heads/" + branch, "sha": commit["sha"],
            })
        except DeliveryError:
            # Another delivery may have created exactly this version concurrently.
            reference = api.request("GET", "repos/" + head_repository + "/git/ref/heads/"
                                    + quote(branch, safe="/"), optional=True)
            if not reference:
                raise
            verify_branch_contents(api, head_repository, reference["object"]["sha"], desired)

    release = plan["release"]
    body = ("Publish Enumeray " + release["version"] + " from its qualified public release.\n\n"
            "Release: https://github.com/" + release["public_repository"]
            + "/releases/tag/" + release["tag"] + "\n"
            "Source revision: `" + release["public_revision"] + "`\n"
            "Cargo.lock SHA-256: `" + release["lockfile_sha256"] + "`\n\n"
            "Package installation starts no connector service. Coordinate matching daemon and runner upgrades.")
    try:
        pr = api.request("POST", "repos/" + repository + "/pulls", {
            "title": "Update Enumeray to " + release["version"],
            "body": body, "head": head_repository.split("/")[0] + ":" + branch,
            "base": base,
        })
    except DeliveryError:
        # A lost create reply or racing retry must not authorize a duplicate PR.
        pr = find_pull_request(api, repository, head_repository, branch, base)
        if not pr:
            raise
        verify_branch_contents(api, head_repository, pr["head"]["sha"], desired)
    return {"state": "pending-review", "url": pr["html_url"]}


def deliver(directory, channel, config, submit=False, api=None):
    plan, entry, desired = load_delivery(directory, channel, config)
    result = {"schema_version": 1, "channel": channel, "version": plan["release"]["version"]}
    if entry["state"] != "ready":
        return {**result, "state": entry["state"], "reason": entry.get("reason", "Channel is not ready")}
    if not desired:
        raise DeliveryError("The channel has no generated package files")
    if not submit:
        return {**result, "state": "planned", "repository": entry["repository"], "branch": entry["branch"],
                "files": sorted(desired)}
    if api is None and not os.environ.get("GH_TOKEN"):
        return {**result, "state": "blocked-configuration", "reason": "Provide a token scoped to this channel"}
    return {**result, **submit_channel(api or GitHub(), plan, entry, desired)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--delivery", required=True, type=Path)
    parser.add_argument("--config", type=Path, default=ROOT / "distribution/channels.json")
    parser.add_argument("--channel", choices=("homebrew",), required=True)
    parser.add_argument("--submit", action="store_true", help="Create or reuse this channel's versioned update PR")
    parser.add_argument("--status", type=Path, help="Write this attempt's channel status outside immutable release assets")
    args = parser.parse_args()
    try:
        result = deliver(args.delivery, args.channel, json.loads(args.config.read_text()), args.submit)
    except (DeliveryError, ValueError, OSError, KeyError, TypeError) as error:
        result = {"schema_version": 1, "channel": args.channel, "state": "failed", "reason": str(error)}
    contents = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.status:
        args.status.parent.mkdir(parents=True, exist_ok=True)
        args.status.write_text(contents)
    print(contents, end="")
    if result["state"] == "failed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
