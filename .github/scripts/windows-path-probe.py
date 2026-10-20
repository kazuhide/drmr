"""One-off categorical diagnosis on a disposable branch; never emit raw tool data."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

config = json.loads(Path(sys.argv[1]).read_text(encoding='utf-8-sig'))
source = Path(config['source'])
work = Path(os.environ['TEMP'])

def emit(value):
    print(json.dumps(value, sort_keys=True), flush=True)

def categories(data):
    lower=data.lower()
    return {name: needle in lower for name, needle in {
        'path_too_long': b'file name too long',
        'access_denied': b'permission denied',
        'access_denied_windows': b'access is denied',
        'open_failed': b'cannot open',
        'file_failure': b'file_failure',
        'write_failed': b'failed to write',
        'create_failed': b'failed to create',
        'no_such_file': b'no such file',
        'invalid_argument': b'invalid argument',
        'sharing_violation': b'sharing violation',
    }.items()}

def call(label, args, **kwargs):
    result=subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=90, check=False, **kwargs)
    emit({'probe': label, 'returncode': result.returncode, 'diagnostic_flags':categories(result.stderr)})
    return result

emit({'probe':'identity', 'mode':config['mode'], 'source_revision':config['sha'], 'temp_path_units':len(str(work))})
if config['mode'] == 'direct':
    fixture=source/'crates/drmr/tests/fixtures/bbb/clip.mp4'
    result=call('fixture-video-copy', ['ffmpeg','-v','error','-i',str(fixture),'-map','0:v:0','-c','copy','-an','-map_metadata','-1','-bitexact','-movflags','delay_moov+default_base_moof+frag_keyframe',str(work/'source.mp4')])
    if result.returncode: raise SystemExit(1)
    fixture=work/'source.mp4'
    for length in (91,157,200,223,235,245,260,280):
        for form in ('dash','hls'):
            base=work/(form+str(length))
            while len(str(base)) < length:
                remaining=length-len(str(base))-1
                if remaining == 0: base=base.with_name(base.name+'x')
                else: base=base/('x'*min(remaining,60))
            base.mkdir(parents=True)
            track=base/'track-0000'
            track.mkdir()
            inp=fixture.as_posix(); out=track.as_posix()
            args=['packager','--mp4_reset_initial_composition_offset_to_zero=false',f'input={inp},stream=0,init_segment={out}/init.mp4,segment_template={out}/seg_$Number$.m4s,playlist_name=track-0000/media.m3u8','--segment_duration','2','--clear_lead','0']
            if form=='dash': args+=['--generate_static_live_mpd','--mpd_output',(base/'manifest.mpd').as_posix()]
            else: args+=['--hls_playlist_type','VOD','--hls_master_playlist_output',(base/'master.m3u8').as_posix()]
            args+=['--single_threaded','--io_cache_size=0']
            result=call(f'direct-{form}-{length}',args)
            emit({'probe':f'inventory-{form}-{length}','root_path_units':len(str(base)),'init_path_units':len(str(track/'init.mp4')),'segment_path_units':len(str(track/'seg_1.m4s')),'manifest_exists':(base/('manifest.mpd' if form=='dash' else 'master.m3u8')).is_file(),'init_exists':(track/'init.mp4').is_file(),'segment_exists':(track/'seg_1.m4s').is_file()})
else:
    wanted={
        'conversion_bundle_plan':'one_private_plan_publishes_and_independently_verifies_both_presentations',
        'encrypted_download_media':'aes128_ts_url_retains_rotation_and_implicit_sequence_vectors_for_offline_plans',
    }
    executables={entry['name']:entry['path'] for entry in config['executables']}
    for name, test in wanted.items():
        result=subprocess.run([executables[name],test,'--exact','--nocapture'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=240,check=False)
        emit({'probe':'focused-'+name,'returncode':result.returncode,'media_tool_failed':b'media-tool-failed' in result.stderr,'test_passed':b'1 passed; 0 failed' in result.stdout})
        for raw in result.stderr.splitlines():
            if raw.startswith(b'DRMR_PROBE '):
                record=json.loads(raw[len(b'DRMR_PROBE '):])
                allowed={'probe','tool','returncode','is_manifest','max_argument_units','stderr_flags'}
                if set(record) != allowed: raise SystemExit(2)
                emit(record)
