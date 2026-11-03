#!/usr/bin/env python3
"""Reuse recent successful source checks only for the same verified inputs.

Artifacts are hints, never a substitute for GitHub's successful workflow status.
A lookup failure always selects fresh verification; callers upload receipt.json
only after their complete verification gate succeeds.
"""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from urllib.parse import quote, urlencode, urlparse
from urllib.request import build_opener, HTTPRedirectHandler, Request
import zipfile
import zlib

SCHEMA_VERSION = 1
MAX_AGE = timedelta(hours=24)
MAX_ARCHIVE_BYTES = 1024 * 1024
MAX_RECEIPT_BYTES = 64 * 1024
MAX_CANDIDATES = 10
LOOKUP_SECONDS = 90
SHA = re.compile(r'[0-9a-f]{40}')
WORKFLOW = re.compile(r'\.github/workflows/[A-Za-z0-9_.-]+\.ya?ml')


class EvidenceError(ValueError):
    pass


class SafeRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        redirected = super().redirect_request(request, fp, code, message, headers, newurl)
        if urlparse(newurl).scheme != 'https':
            raise EvidenceError('Artifact redirect is not HTTPS')
        if urlparse(request.full_url).netloc != urlparse(newurl).netloc:
            redirected.remove_header('Authorization')
        return redirected


class GitHub:
    def __init__(self, repository, token, api_url='https://api.github.com'):
        self.repository = repository
        self.base = api_url.rstrip('/') + '/repos/' + repository
        self.token = token
        self.opener = build_opener(SafeRedirect())
        self.deadline = time.monotonic() + LOOKUP_SECONDS

    def read(self, path, *, limit=4 * MAX_ARCHIVE_BYTES):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise EvidenceError('Evidence lookup time budget exhausted')
        request = Request(self.base + path, headers={
            'Authorization': 'Bearer ' + self.token,
            'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2022-11-28',
        })
        with self.opener.open(request, timeout=min(10, remaining)) as response:
            data = response.read(limit + 1)
        if len(data) > limit:
            raise EvidenceError('GitHub response exceeds the evidence size limit')
        return data

    def get(self, path):
        return json.loads(self.read(path))

    def receipt(self, run, artifact):
        name = artifact + '-attempt-' + str(run['run_attempt'])
        response = self.get('/actions/runs/' + str(run['id']) + '/artifacts?per_page=100')
        matches = [item for item in response['artifacts'] if item.get('name') == name]
        if len(matches) != 1:
            raise EvidenceError('Successful run has no unique evidence artifact')
        item = matches[0]
        if item.get('expired') is not False or not 0 < item['size_in_bytes'] <= MAX_ARCHIVE_BYTES:
            raise EvidenceError('Evidence artifact is expired or oversized')
        data = self.read('/actions/artifacts/' + str(item['id']) + '/zip', limit=MAX_ARCHIVE_BYTES)
        digest = item.get('digest')
        if digest is not None and digest != 'sha256:' + hashlib.sha256(data).hexdigest():
            raise EvidenceError('Evidence archive digest does not match GitHub metadata')
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            entries = archive.infolist()
            if len(entries) != 1 or entries[0].filename != 'receipt.json':
                raise EvidenceError('Evidence archive must contain only receipt.json')
            entry = entries[0]
            if not 0 < entry.file_size <= MAX_RECEIPT_BYTES or entry.flag_bits & 1:
                raise EvidenceError('Evidence receipt is oversized or encrypted')
            return json.loads(archive.read(entry))


def timestamp(value):
    if not isinstance(value, str):
        raise EvidenceError('Evidence timestamp must be a string')
    result = datetime.fromisoformat(value.replace('Z', '+00:00'))
    if result.tzinfo is None:
        raise EvidenceError('Evidence timestamp must include its timezone')
    return result


def recent(value, now):
    age = now - timestamp(value)
    return timedelta(0) <= age <= MAX_AGE


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def context_identity(environ):
    context = {key: environ.get(key, '') for key in (
        'RUNNER_OS', 'RUNNER_ARCH', 'RUNNER_ENVIRONMENT', 'ImageOS', 'ImageVersion')}
    context['configuration'] = json.loads(environ.get('CI_EVIDENCE_CONTEXT', '{}'))
    context['policy_version'] = SCHEMA_VERSION
    context['python_version'] = sys.version
    return hashlib.sha256(json.dumps(context, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def current_receipt(root, workflow, checks, environ, now):
    if not WORKFLOW.fullmatch(workflow):
        raise EvidenceError('Expected a repository-local workflow path')
    if not isinstance(checks, list) or any(not isinstance(check, str) or not check for check in checks):
        raise EvidenceError('Required checks must be a JSON list of check names')
    if git(root, 'status', '--porcelain', '--untracked-files=normal', '--ignore-submodules=none'):
        raise EvidenceError('Source evidence requires a clean checkout and public dependency')
    submodules = subprocess.check_output(['git', '-C', str(root), 'submodule', 'status', '--recursive'], text=True)
    if any(line and line[0] != ' ' for line in submodules.splitlines()):
        raise EvidenceError('Source evidence requires initialized matching submodules')
    sha = git(root, 'rev-parse', 'HEAD')
    if not SHA.fullmatch(sha) or sha != environ['GITHUB_SHA']:
        raise EvidenceError('Checkout does not match the current workflow commit')
    run_id, attempt = int(environ['GITHUB_RUN_ID']), int(environ['GITHUB_RUN_ATTEMPT'])
    if run_id <= 0 or attempt <= 0:
        raise EvidenceError('Invalid current workflow identity')
    return {
        'schema_version': SCHEMA_VERSION,
        'repository': environ['GITHUB_REPOSITORY'], 'workflow': workflow,
        'run_id': run_id, 'run_attempt': attempt,
        'event': environ['GITHUB_EVENT_NAME'], 'ref': environ.get('GITHUB_REF', ''),
        'base_ref': environ.get('GITHUB_BASE_REF', ''),
        'sha': sha, 'tree': git(root, 'rev-parse', 'HEAD^{tree}'),
        'checks': sorted(set(checks)), 'context': context_identity(environ),
        'verified_at': now.isoformat(), 'source_run': run_id, 'reused_from': None,
    }


def eligible_run(run, current, exact_sha, now):
    if (not isinstance(run, dict) or type(run.get('id')) is not int or type(run.get('run_attempt')) is not int
            or run['id'] == current['run_id'] or run['run_attempt'] <= 0
            or run.get('status') != 'completed' or run.get('conclusion') != 'success'
            or str(run.get('path', '')).split('@', 1)[0] != current['workflow']
            or (run.get('repository') or {}).get('full_name') != current['repository']
            or (run.get('head_repository') or {}).get('full_name') != current['repository']
            or not recent(run.get('updated_at'), now)):
        return False
    if run.get('event') in ('push', 'workflow_dispatch'):
        return run.get('head_branch') == 'main' and run.get('head_sha') == current['sha']
    return not exact_sha and run.get('event') == 'pull_request'


def validate_receipt(receipt, run, current, exact_sha, now, api):
    if (not isinstance(receipt, dict) or type(receipt.get('schema_version')) is not int
            or receipt['schema_version'] != SCHEMA_VERSION):
        raise EvidenceError('Unsupported source evidence schema')
    for key in ('repository', 'workflow', 'tree', 'context'):
        if receipt.get(key) != current[key]:
            raise EvidenceError('Source evidence input changed: ' + key)
    for key, expected in (('run_id', run['id']), ('run_attempt', run['run_attempt'])):
        if type(receipt.get(key)) is not int or receipt[key] != expected:
            raise EvidenceError('Source evidence does not belong to this workflow attempt')
    if receipt.get('event') != run['event'] or not recent(receipt.get('verified_at'), now):
        raise EvidenceError('Source evidence event changed or original tests are stale')
    checks = receipt.get('checks')
    if (not isinstance(checks, list) or any(not isinstance(check, str) for check in checks)
            or not set(current['checks']).issubset(checks)):
        raise EvidenceError('Successful evidence does not cover every required check')
    sha = receipt.get('sha')
    if not isinstance(sha, str) or not SHA.fullmatch(sha):
        raise EvidenceError('Source evidence has no exact checked-out commit')
    if run['event'] in ('push', 'workflow_dispatch'):
        if sha != current['sha'] or receipt.get('ref') != 'refs/heads/main':
            raise EvidenceError('Main evidence is for a different revision')
    else:
        if exact_sha or receipt.get('base_ref') != 'main':
            raise EvidenceError('Pull request evidence is not eligible here')
        commit = api.get('/git/commits/' + sha)
        parents = [parent['sha'] for parent in commit['parents']]
        if commit['tree']['sha'] != current['tree']:
            raise EvidenceError('Pull request checkout has a different source tree')
        # Resolve server PR state even when workflow history embeds associations.
        # Unmerged same-repository workflows are not trusted to mint evidence for
        # an arbitrary main tree by inventing a synthetic commit and receipt.
        pulls = api.get('/commits/' + quote(run['head_sha']) + '/pulls?per_page=100')
        if not isinstance(pulls, list):
            raise EvidenceError('Malformed pull request association')
        if not any(parents == [pull['base']['sha'], pull['head']['sha']]
                   and same_pull_repository(pull, run)
                   and pull.get('merged_at')
                   and pull.get('merge_commit_sha') == current['sha']
                   and pull.get('base', {}).get('repo', {}).get('full_name') == current['repository']
                   and pull.get('base', {}).get('ref') == 'main'
                   and run.get('head_sha') == pull['head']['sha']
                   and receipt.get('ref') == 'refs/pull/' + str(pull['number']) + '/merge'
                   for pull in pulls):
            raise EvidenceError('Pull request checkout is not the server-recorded merge result')
    return receipt


def same_pull_repository(pull, run):
    # Full PR metadata must independently confirm the source repository.
    repo = pull.get('head', {}).get('repo')
    if not repo:
        return False
    expected = run.get('repository') or {}
    if repo.get('full_name') is not None:
        return repo['full_name'] == expected.get('full_name')
    return repo.get('id') is not None and repo['id'] == expected.get('id')


def find_evidence(api, current, artifact, exact_sha, now):
    # Bounded history is an optimization. Older/missing evidence runs tests.
    candidates = 0
    for page in range(1, 4):
        query = {'status': 'success', 'per_page': 100, 'page': page}
        if exact_sha:
            query['head_sha'] = current['sha']
        endpoint = '/actions/workflows/' + quote(Path(current['workflow']).name) + '/runs?'
        runs = api.get(endpoint + urlencode(query))['workflow_runs']
        if not isinstance(runs, list):
            raise EvidenceError('Malformed workflow history')
        for run in runs:
            try:
                if not eligible_run(run, current, exact_sha, now):
                    continue
                candidates += 1
                if candidates > MAX_CANDIDATES:
                    return None
                receipt = api.receipt(run, artifact)
                validate_receipt(receipt, run, current, exact_sha, now, api)
            except (EvidenceError, ValueError, KeyError, TypeError, AttributeError, OSError,
                    zipfile.BadZipFile, NotImplementedError, RuntimeError, zlib.error):
                continue
            return receipt, run
        if len(runs) < 100:
            break
    return None


def lookup(root, workflow, artifact, checks, environ, *, exact_sha=False, now=None, api=None):
    now = now or datetime.now(timezone.utc)
    current = current_receipt(root, workflow, checks, environ, now)
    eligible_event = (current['event'] == 'push'
                      and (current['ref'] == 'refs/heads/main' or exact_sha))
    if not eligible_event:
        return current, False
    token = environ.get('GH_TOKEN') or environ.get('GITHUB_TOKEN')
    if not token and api is None:
        return current, False
    try:
        api = api or GitHub(current['repository'], token, environ.get('GITHUB_API_URL', 'https://api.github.com'))
        match = find_evidence(api, current, artifact, exact_sha, now)
        if match:
            receipt, run = match
            current.update(verified_at=receipt['verified_at'], source_run=run['id'],
                           reused_from={'run_id': run['id'], 'run_attempt': run['run_attempt'], 'sha': receipt['sha']})
            return current, True
    except (EvidenceError, ValueError, KeyError, TypeError, AttributeError, OSError,
                    zipfile.BadZipFile, NotImplementedError, RuntimeError, zlib.error):
        print('Source evidence is unavailable; running fresh verification.', flush=True)
    return current, False


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['lookup'])
    parser.add_argument('--workflow', required=True)
    parser.add_argument('--artifact', required=True)
    parser.add_argument('--checks', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--exact-sha', action='store_true')
    args = parser.parse_args(argv)
    try:
        root = Path(subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], text=True).strip())
        receipt, reused = lookup(root, args.workflow, args.artifact,
                                 json.loads(args.checks.read_text()), os.environ, exact_sha=args.exact_sha)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(receipt, indent=2) + '\n')
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write('reused=' + str(reused).lower() + '\n')
            output.write('source_run=' + (str(receipt['source_run']) if reused else '') + '\n')
        print('Reusing successful source verification from run ' + str(receipt['source_run'])
              if reused else 'Running fresh source verification.')
        return 0
    except (EvidenceError, ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print('ERROR: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
