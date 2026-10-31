#!/usr/bin/python3
"""Bounded, disposable Linux host-apply checks; no native daemon lifecycle claim."""
import copy
import hashlib
import json
import os
from pathlib import Path
import pwd
import subprocess

EXE = '/usr/local/bin/enumeray'
BASE = Path('/var/lib/enumeray-guided-checks')
BASE.mkdir(mode=0o700)
source = Path('/opt/enumeray/examples/host-setup-linux.json')
original = json.loads(source.read_text())
checksum = hashlib.sha256(Path(EXE).read_bytes()).hexdigest()

def invoke(argv, success=True):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=150)
    assert (result.returncode == 0) == success, (argv, result.stdout, result.stderr)
    return result

for driver in ('process', 'slurm'):
    for connection in ('direct', 'ssh'):
        identifier = f'guided-{driver}-{connection}'
        spec = copy.deepcopy(original)
        spec['context']['id'] = identifier
        spec['context']['connector_id'] = identifier
        spec['connector_account'].update(name='enumeray-guided', group='enumeray-guided', home='/var/lib/enumeray-guided', create=True)
        spec['execution_account'].update(name='model', group='model', home='/home/model', create=False)
        spec['connector_executable'] = '/opt/enumeray/guided-bin/enumeray'
        spec['executable_source'] = EXE
        spec['executable_sha256'] = checksum
        spec['install_service'] = True
        spec['journal'] = '/var/lib/enumeray-guided/' + identifier + '/journal'
        spec['token_file'] = '/var/lib/enumeray-guided/' + identifier + '/credentials/token'
        spec['context']['definition']['work_root'] = '/srv/' + identifier
        spec['context']['definition']['installation_roots'] = ['/opt/enumeray/' + identifier]
        if driver == 'slurm':
            spec['context']['definition']['driver'] = {'kind':'slurm','worker_path':EXE}
        if connection == 'direct':
            spec['context']['definition']['connection']['launcher'] = ['/usr/bin/sudo','-n','-u','model','--',spec['connector_executable']]
        else:
            spec['context']['definition']['connection'] = {'kind':'ssh','host':'127.0.0.1','port':2222,'user':'model','credential_reference':'fixture-key','runner_path':spec['connector_executable']}
            spec['credentials_file'] = '/var/lib/enumeray-guided/' + identifier + '/ssh.json'
        path = BASE / (identifier + '.json')
        path.write_text(json.dumps(spec))
        plan = BASE / identifier
        invoke([EXE,'connector','configuration','generate','--file',str(path),'--directory',str(plan)])
        # The approval is SHA-256 of the typed spec's deterministic serialized form,
        # exposed by setup rather than recomputed using Python object ordering.
        setup_input = BASE / (identifier + '-input.json')
        setup_input.write_text(json.dumps({'manifest_version':1,'host':spec,'connector':{'id':identifier,'label':'Host fixture','max_outstanding_assignments':1},'startup':'background'}))
        owner = BASE / (identifier + '-owner')
        review = invoke([EXE,'--json','setup','--non-interactive','--file',str(setup_input),'--directory',str(owner)], success=False)
        approval = json.loads(review.stdout)['host_plan_sha256']
        apply = [EXE,'setup','apply-host','--plan',str(plan/'plan.json'),'--side','connector','--approve',approval]
        invoke(apply)
        runtime = Path(spec['journal']).parent / 'runtime.json'
        retained = runtime.read_bytes()
        assert runtime.stat().st_mode & 0o077 == 0
        assert runtime.stat().st_uid == pwd.getpwnam('enumeray-guided').pw_uid
        invoke(apply)
        assert runtime.read_bytes() == retained
        assert not (Path(spec['journal'])/'journal.json').exists(), 'host apply must not start a connector'
        assert not Path(spec['token_file']).exists(), 'host apply without enrollment must not invent a token'
        invalid_enrollment = BASE / (identifier + '-invalid-enrollment.json')
        invalid_enrollment.write_text(json.dumps({'id':'00000000-0000-0000-0000-000000000001','connector_id':identifier,'enrollment_token':'invalid','expires_at':'2099-01-01T00:00:00Z'}))
        invalid_enrollment.chmod(0o600)
        invoke([*apply,'--enrollment-file',str(invalid_enrollment)], success=False)
        assert not Path('/var/lib/enumeray-guided/enrollment-00000000-0000-0000-0000-000000000001.json').exists()
        if connection == 'ssh':
            invoke([EXE,'setup','apply-host','--plan',str(plan/'plan.json'),'--side','execution','--approve',approval])
        else:
            denied = ['sudo','-u','enumeray-guided','sudo','-n','-u','model','--',spec['connector_executable'],'connector','run','--help']
            invoke(denied, success=False)
        unit = Path('/etc/systemd/system') / ('enumeray-' + identifier + '.service')
        before = unit.read_bytes()
        unit.write_bytes(before + b'# conflicting site modification\n')
        invoke(apply, success=False)
        assert unit.read_bytes().endswith(b'# conflicting site modification\n')
        unit.write_bytes(before)
print(json.dumps({'guided_host_apply':'passed','paths':4,'account_creation':True,'repeated_apply':True,'conflicts_preserved':True,'daemon_lifecycle':'unverified'}), flush=True)

# Source fixtures for matching-service guided candidate trials. The owner CLI and
# connector use enumeray's OS account; model execution remains the distinct model user.
plans = Path('/opt/enumeray/guided-host-plans')
releases = Path('/opt/enumeray/guided-releases')
plans.mkdir(mode=0o755)
releases.mkdir(mode=0o755)
plans.chmod(0o755)
releases.chmod(0o755)
for driver in ('process', 'slurm'):
    for connection in ('direct', 'ssh'):
        identifier = f'journey-{driver}-{connection}'
        spec = copy.deepcopy(original)
        spec['api_base_url'] = 'http://127.0.0.1:8180'
        spec['context'].update(id=identifier, connector_id=identifier)
        spec['connector_account'].update(name='enumeray',group='enumeray',home='/home/enumeray',create=False)
        spec['execution_account'].update(name='model',group='model',home='/home/model',create=False)
        spec['connector_executable'] = EXE
        spec['executable_source'] = EXE
        spec['executable_sha256'] = checksum
        spec['install_service'] = False
        spec['journal'] = f'/var/lib/enumeray/guided/{identifier}/journal'
        spec['token_file'] = f'/var/lib/enumeray/guided/{identifier}/credentials/token'
        spec['context']['definition']['work_root'] = '/srv/' + identifier
        spec['context']['definition']['installation_roots'] = [str(releases)]
        if driver == 'slurm':
            spec['context']['definition']['driver'] = {'kind':'slurm','worker_path':EXE}
        if connection == 'direct':
            spec['context']['definition']['connection']['launcher'] = ['/usr/bin/sudo','-n','-u','model','--',EXE]
        else:
            spec['context']['definition']['connection'] = {'kind':'ssh','host':'127.0.0.1','port':2222,'user':'model','credential_reference':'fixture-key','runner_path':EXE}
            spec['credentials_file'] = f'/var/lib/enumeray/guided/{identifier}/ssh.json'
        document = plans / (identifier + '.json')
        document.write_text(json.dumps(spec))
        document.chmod(0o644)
        private = BASE / (identifier + '-plan')
        invoke([EXE,'connector','configuration','generate','--file',str(document),'--directory',str(private)])
        setup_input = BASE / (identifier + '-input.json')
        setup_input.write_text(json.dumps({'manifest_version':1,'host':spec,'connector':{'id':identifier,'label':'Guided journey','max_outstanding_assignments':1},'startup':'foreground'}))
        review=invoke([EXE,'--json','setup','--file',str(setup_input),'--directory',str(BASE/(identifier+'-owner')),'--non-interactive'],success=False)
        approval=json.loads(review.stdout)['host_plan_sha256']
        invoke([EXE,'setup','apply-host','--plan',str(private/'plan.json'),'--side','connector','--approve',approval])
        project=releases/identifier
        invoke([EXE,'model','init','guided-model-'+driver+'-'+connection,'--directory',str(project),'--driver',driver])
        if driver=='slurm':
            batch=project/'model.sbatch'
            batch.write_text(batch.read_text().replace('#SBATCH --nodes','#SBATCH --account=research\n#SBATCH --partition=debug\n#SBATCH --clusters=enumeray-acceptance\n#SBATCH --nodes'))
        for folder, dirs, files in os.walk(project):
            Path(folder).chmod(0o755)
            for name in files:
                path=Path(folder)/name
                path.chmod(0o555 if path.stat().st_mode&0o111 else 0o444)
print(json.dumps({'guided_journey_host_fixtures':'prepared','paths':4}),flush=True)
