"""Source-bound qualification; no raw subprocess output enters logs or artifacts."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

config = json.loads(Path(sys.argv[1]).read_text(encoding='utf-8-sig'))
source = Path(config['source'])
report_path = Path(sys.argv[1]).parent/'report.json'
report = {'schema_version':1,'product_source_head':config['product_head'],'product_source_tree':config['product_tree'],'workflow_head':config['workflow_head'],'passed':False,'regular_user_verified':True,'source':{},'tools':{},'tests':[]}

class Refusal(Exception):
    pass

def require(condition):
    if not condition: raise Refusal()

def digest(path):
    result=hashlib.sha256()
    with path.open('rb') as stream:
        while block:=stream.read(1024*1024): result.update(block)
    return result.hexdigest()

def run(args, timeout=600):
    return subprocess.run(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout,check=False)

def git(*args):
    result=run(['git','-c','safe.directory='+str(source),'-C',str(source),*args],60)
    require(result.returncode==0)
    return result.stdout

def is_runtime(path):
    return path.startswith(('crates/','.cargo/')) or path in ('Cargo.toml','Cargo.lock','rust-toolchain.toml','rust-toolchain')

def fingerprint():
    excluded={'.github/workflows/windows.yml','.github/scripts/windows-green-probe.py','.github/scripts/windows-green-probe.ps1'}
    entries=[]
    for row in git('ls-files','--stage','-z').split(b'\0'):
        if not row: continue
        meta,rawpath=row.split(b'\t',1)
        mode,index_oid,stage=meta.decode().split()
        path=rawpath.decode('utf-8')
        require(stage=='0')
        if path in excluded: continue
        data=(source/path).read_bytes()
        actual=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
        require(actual==index_oid)
        entries.append([mode,actual,path])
    entries.sort(key=lambda x:x[2].encode())
    runtime=[x for x in entries if is_runtime(x[2])]
    encode=lambda rows:hashlib.sha256(json.dumps(rows,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
    value={'all_source_sha256':encode(entries),'all_source_files':len(entries),'runtime_sha256':encode(runtime),'runtime_files':len(runtime)}
    require(value['all_source_sha256']==config['source_fingerprint'])
    require(value['runtime_sha256']==config['runtime_fingerprint'])
    require(len(entries)==config['source_count'] and len(runtime)==config['runtime_count'])
    require(git('rev-parse','HEAD').decode().strip()==config['workflow_head'])
    require(not git('status','--porcelain','--untracked-files=all').strip())
    return value

name_pattern=re.compile(r'^[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*$')
summary_pattern=re.compile(rb'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;',re.M)

def listed(executable, selection, ignored=False):
    args=[executable,*selection,'--list']
    if ignored: args.append('--ignored')
    result=run(args,60)
    require(result.returncode==0)
    names=[]
    for raw in result.stdout.decode('utf-8').splitlines():
        if raw.endswith(': test'):
            name=raw[:-6]
            require(name_pattern.fullmatch(name) is not None)
            names.append(name)
    return names

def qualify(name, executable, selection, expected_count=None, minimum_count=None):
    names=listed(executable,selection)
    require(names and not listed(executable,selection,True))
    if expected_count is not None: require(len(names)==expected_count)
    if minimum_count is not None: require(len(names)>=minimum_count)
    desired={
        'encrypted-download-all': {
            'aes128_ts_url_retains_rotation_and_implicit_sequence_vectors_for_offline_plans',
            'aes128_fragmented_url_retains_short_explicit_vectors_for_offline_plans',
            'sample_aes_ts_url_retains_fairplay_identifiers_for_offline_plans',
            'cbcs_fragmented_url_uses_mp4_input_key_ids_for_offline_plans',
            'cenc_fragmented_url_matches_existing_local_input_admission_for_offline_plans',
        },
        'native-manifest-cli-all': {'external_validation_requires_its_explicit_flag_and_failure_retains_native_evidence'},
        'destination-guard-units': {
            'conversion::plan::destination::tests::ordinary_output_stages_use_the_requested_destination_parent',
            'conversion::plan::destination::tests::bundle_output_stages_are_siblings_and_intermediate_stages_keep_their_own_parent',
            'conversion::plan::destination::tests::sibling_stage_publication_waits_for_the_outer_bundle_commit',
            'conversion::plan::destination::tests::destination_ancestors_are_pinned_against_rename_until_the_guard_is_released',
        },
        'conformance-workspace-units': {
            'conversion::conformance::workspace::tests::root_and_nested_copies_remain_strict_inside_the_media_workspace',
            'conversion::conformance::workspace::tests::an_existing_copy_is_never_overwritten',
            'conversion::conformance::workspace::tests::a_changed_nested_copy_is_rejected_even_when_its_length_is_unchanged',
        },
    }.get(name,set())
    require(desired.issubset(names))
    started=time.monotonic()
    result=run([executable,*selection,'--nocapture'])
    summaries=summary_pattern.findall(result.stdout)
    outer=summaries[-1] if summaries else None
    passed=bool(result.returncode==0 and outer and outer[0]==b'ok' and int(outer[1])==len(names) and all(int(x[2])==0 and int(x[3])==0 for x in summaries) and b'skipped:' not in result.stdout+result.stderr and not re.search(rb'^test \S+ \.\.\. (?:FAILED|ignored)',result.stdout,re.M))
    entry={'name':name,'selection':selection,'listed_tests':names,'listed_count':len(names),'passed':passed,'returncode':result.returncode,'elapsed_seconds':round(time.monotonic()-started,3),'stdout_sha256':hashlib.sha256(result.stdout).hexdigest(),'stderr_sha256':hashlib.sha256(result.stderr).hexdigest(),'summaries':[{'status':x[0].decode(),'passed':int(x[1]),'failed':int(x[2]),'ignored':int(x[3]),'measured':int(x[4]),'filtered_out':int(x[5])} for x in summaries],'fixed_error_flags':{code:code.encode() in result.stdout+result.stderr for code in ('media-tool-failed','manifest-validation-workspace-failed','manifest-validator-unavailable','temporary-workspace-unavailable','output-staging-failed')}}
    report['tests'].append(entry)
    print(json.dumps({'qualification':'test','name':name,'listed_count':len(names),'passed':passed,'returncode':result.returncode,'summaries':entry['summaries']},sort_keys=True),flush=True)
    return passed

try:
    report['source']['before']=fingerprint()
    report['compiler']={'rustc':config['rustc'].strip(),'cargo':config['cargo'].strip()}
    report['temp_path_units']=len(os.environ['TEMP'].encode('utf-16-le'))//2
    tools=Path(config['tools'])
    for name,args in [('packager',['--version']),('ffmpeg',['-hide_banner','-version']),('ffprobe',['-hide_banner','-version'])]:
        executable=tools/(name+'.exe');result=run([str(executable),*args],30)
        require(result.returncode==0)
        lines=(result.stdout+result.stderr).decode('utf-8','replace').splitlines()
        require(lines and len(lines[0])<=512 and not any(ord(c)<32 for c in lines[0]))
        report['tools'][name]={'sha256':digest(executable),'version':lines[0]}
    require(report['tools']['packager']['sha256']=='e9b18cbb9d2c3c1290340b727fa3c5d72d3ab58fa822007767b2d4c4040d0e3a')
    for name in ('mp4dump','mp4encrypt'):
        report['tools'][name]={'sha256':digest(tools/(name+'.exe'))}
    executables={x['name']:x['path'] for x in config['executables']}
    required={'conversion_bundle_plan','encrypted_download_media','native_manifest_cli','drmr_core'}
    require(set(executables)==required)
    report['executables']={name:{'sha256':digest(Path(path))} for name,path in sorted(executables.items())}
    checks=[]
    checks.append(qualify('bundle-exact-success',executables['conversion_bundle_plan'],['one_private_plan_publishes_and_independently_verifies_both_presentations','--exact'],expected_count=1))
    checks.append(qualify('encrypted-download-all',executables['encrypted_download_media'],[],expected_count=config['expected_encrypted']))
    checks.append(qualify('native-manifest-cli-all',executables['native_manifest_cli'],[],expected_count=config['expected_manifest']))
    checks.append(qualify('conformance-workspace-units',executables['drmr_core'],['conversion::conformance::workspace::tests::'],expected_count=3))
    checks.append(qualify('destination-guard-units',executables['drmr_core'],['conversion::plan::destination::tests::'],minimum_count=4))
    report['source']['after']=fingerprint()
    require(report['source']['before']==report['source']['after'])
    report['passed']=all(checks)
except Exception as error:
    report['harness_failure']={'classification':'qualification-refused' if isinstance(error,Refusal) else 'harness-operation-failed','exception_type':type(error).__name__ if type(error).__name__ in ('Refusal','OSError','FileNotFoundError','PermissionError','TimeoutExpired','KeyError','ValueError','UnicodeDecodeError') else 'other'}
finally:
    report_path.write_text(json.dumps(report,indent=2,sort_keys=True)+'\n',encoding='utf-8')
    print(json.dumps({'qualification':'final','passed':report['passed'],'product_source_head':config['product_head'],'workflow_head':config['workflow_head'],'test_groups_recorded':len(report['tests']),'harness_failure':report.get('harness_failure')},sort_keys=True),flush=True)
raise SystemExit(0 if report['passed'] else 1)
