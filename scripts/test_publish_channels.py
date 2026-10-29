"""Delivery retries preserve immutable version content and channel isolation."""
import base64
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
from urllib.parse import parse_qs, unquote

from test_generate_channels import channels, config_fixture, manifest_fixture


spec = importlib.util.spec_from_file_location("publish_channels", Path(__file__).with_name("publish-channels.py"))
publishing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publishing)


class FakeGitHub:
    def __init__(self):
        self.calls = []
        self.writes = []
        self.refs = {}
        self.trees = {"base-tree": {}}
        self.commits = {"base": {"tree": {"sha": "base-tree"}}}
        self.prs = []
        self.lose_pr_response = False
        self.race_ref_response = False

    def request(self, method, endpoint, payload=None, optional=False):
        self.calls.append((method, endpoint, copy.deepcopy(payload)))
        path, _, query = endpoint.partition("?")
        if method != "GET":
            self.writes.append((method, endpoint, copy.deepcopy(payload)))
        _, owner, name, suffix = path.split("/", 3)
        repository = owner + "/" + name
        if method == "GET" and suffix == "pulls":
            head = parse_qs(query)["head"][0]
            return [copy.deepcopy(pr) for pr in self.prs if pr["head_label"] == head and pr["repository"] == repository]
        if method == "GET" and suffix.startswith("git/ref/heads/"):
            branch = unquote(suffix.removeprefix("git/ref/heads/"))
            if branch == "main":
                return {"object": {"sha": "base"}}
            sha = self.refs.get((repository, branch))
            if sha:
                return {"object": {"sha": sha}}
            if optional:
                return None
        if method == "GET" and suffix.startswith("git/commits/"):
            return copy.deepcopy(self.commits[suffix.rsplit("/", 1)[1]])
        if method == "GET" and suffix.startswith("contents/"):
            revision = parse_qs(query)["ref"][0]
            path = unquote(suffix.removeprefix("contents/"))
            tree = self.trees[self.commits[revision]["tree"]["sha"]]
            content = tree.get(path)
            if content is None and optional:
                return None
            return {"content": base64.b64encode(content.encode()).decode()}
        if method == "POST" and suffix == "git/trees":
            tree_id = "tree-" + str(len(self.trees))
            self.trees[tree_id] = {**self.trees[payload["base_tree"]],
                                   **{item["path"]: item["content"] for item in payload["tree"]}}
            return {"sha": tree_id}
        if method == "POST" and suffix == "git/commits":
            commit_id = "commit-" + str(len(self.commits))
            self.commits[commit_id] = {"tree": {"sha": payload["tree"]}}
            return {"sha": commit_id}
        if method == "POST" and suffix == "git/refs":
            branch = payload["ref"].removeprefix("refs/heads/")
            self.refs[(repository, branch)] = payload["sha"]
            if self.race_ref_response:
                self.race_ref_response = False
                raise publishing.DeliveryError("racing ref response")
            return {"object": {"sha": payload["sha"]}}
        if method == "POST" and suffix == "pulls":
            owner, branch = payload["head"].split(":", 1)
            source = next(repo for repo, ref in self.refs if repo.split("/")[0] == owner and ref == branch)
            pr = {"html_url": "https://github.com/" + repository + "/pull/1", "state": "open",
                  "merged_at": None, "head": {"sha": self.refs[(source, branch)]},
                  "head_label": payload["head"], "repository": repository}
            self.prs.append(pr)
            if self.lose_pr_response:
                self.lose_pr_response = False
                raise publishing.DeliveryError("lost PR create response")
            return copy.deepcopy(pr)
        raise AssertionError("Unexpected API operation: " + method + " " + endpoint)


class PublishChannelTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.config = config_fixture()
        artifacts, plan = channels.generate(manifest_fixture(), self.config)
        channels.write_delivery(self.directory, artifacts, plan)
        self.api = FakeGitHub()

    def deliver(self, channel="homebrew", submit=True):
        return publishing.deliver(self.directory, channel, self.config, submit=submit, api=self.api)

    def test_plan_is_read_only_without_submit_even_when_an_api_is_supplied(self):
        result = self.deliver(submit=False)
        self.assertEqual(result["state"], "planned")
        self.assertEqual(self.api.calls, [])

    def test_retry_reuses_exact_branch_and_pr_without_more_writes(self):
        first = self.deliver()
        writes = copy.deepcopy(self.api.writes)
        retry = self.deliver()
        self.assertEqual(first["state"], "pending-review")
        self.assertEqual(first, retry)
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(len(self.api.prs), 1)

    def test_merged_update_is_delivered_even_after_its_branch_is_deleted(self):
        self.deliver()
        self.api.prs[0].update(state="closed", merged_at="2026-10-29T12:00:00Z")
        self.api.refs.clear()
        self.assertEqual(self.deliver()["state"], "delivered")

    def test_closed_unmerged_update_needs_attention_instead_of_a_duplicate(self):
        self.deliver()
        self.api.prs[0]["state"] = "closed"
        self.assertEqual(self.deliver()["state"], "needs-maintainer")
        self.assertEqual(len(self.api.prs), 1)

    def test_conflicting_existing_version_cannot_be_force_updated(self):
        self.deliver()
        revision = self.api.prs[0]["head"]["sha"]
        tree = self.api.trees[self.api.commits[revision]["tree"]["sha"]]
        tree["Formula/enumeray.rb"] = "different bytes under the same version"
        before = copy.deepcopy(self.api.writes)
        with self.assertRaisesRegex(publishing.DeliveryError, "Conflicting"):
            self.deliver()
        self.assertEqual(self.api.writes, before)

    def test_lost_pr_create_reply_is_reconciled_without_duplicate(self):
        self.api.lose_pr_response = True
        self.assertEqual(self.deliver()["state"], "pending-review")
        self.assertEqual(len(self.api.prs), 1)
        self.assertEqual(sum(1 for _, endpoint, _ in self.api.writes if endpoint.endswith("/pulls")), 1)

    def test_concurrent_same_branch_creation_is_accepted_only_with_same_bytes(self):
        self.api.race_ref_response = True
        self.assertEqual(self.deliver()["state"], "pending-review")
        self.assertEqual(len(self.api.prs), 1)

    def test_pending_conda_review_is_separate_from_the_homebrew_update(self):
        brew = self.deliver()
        conda = self.deliver(channel="conda")
        self.assertEqual(brew["state"], "pending-review")
        self.assertEqual(conda["state"], "pending-review")
        self.assertNotEqual(brew["url"], conda["url"])
        self.assertEqual(len(self.api.prs), 2)
        conda_tree = self.api.trees[self.api.commits[self.api.prs[1]["head"]["sha"]]["tree"]["sha"]]
        self.assertTrue(all(path.startswith("recipes/enumeray/") for path in conda_tree))

    def test_missing_conda_configuration_does_not_block_formula_generation(self):
        self.config["conda"]["maintainers"] = []
        with tempfile.TemporaryDirectory() as temporary:
            artifacts, plan = channels.generate(manifest_fixture(), self.config)
            channels.write_delivery(temporary, artifacts, plan)
            result = publishing.deliver(temporary, "conda", self.config, submit=True, api=self.api)
            self.assertEqual(result["state"], "blocked-configuration")
            self.assertEqual(publishing.deliver(temporary, "homebrew", self.config, submit=False)["state"], "planned")
            self.assertEqual(self.api.calls, [])

    def test_changed_generated_file_is_rejected_before_github_access(self):
        (self.directory / "homebrew/Formula/enumeray.rb").write_text("tampered")
        with self.assertRaisesRegex(publishing.DeliveryError, "changed after planning"):
            self.deliver()
        self.assertEqual(self.api.calls, [])

    def test_raw_provider_error_cannot_leak_a_token_into_status(self):
        response = subprocess.CompletedProcess([], 1, stdout="", stderr="GH_TOKEN=private-token HTTP 401")
        with mock.patch.object(publishing.subprocess, "run", return_value=response):
            with self.assertRaises(publishing.DeliveryError) as error:
                publishing.GitHub().request("GET", "repos/owner/repository")
        self.assertNotIn("private-token", str(error.exception))


if __name__ == "__main__":
    unittest.main()
