import sys, os, subprocess, json
sys.path.insert(0,'E:/workspaces/Cisox/snow-shot-rs/tools/snow-translator/eval')
import run_quant_matrix as r
M='E:/models/translate-eval/'
U='nllb600m-pruned-un6-ccm-int4'; X='nllb600m-pruned-main14-ccm-int4'
J=[('un6_base',U,'tok_un6ccm.json',[]),('un6_share',U,'tok_un6ccm.json',['--share-emb']),
 ('un6_arenaoff',U,'tok_un6ccm.json',['--arena','off']),('un6_beam2',U,'tok_un6ccm.json',['--beams','2']),
 ('un6_beam1',U,'tok_un6ccm.json',['--beams','1']),('un6_maxnew64',U,'tok_un6ccm.json',['--max-new','64']),
 ('un6_sameasreq',U,'tok_un6ccm.json',['--prov','arena_extend_strategy=kSameAsRequested']),
 ('un6_envalloc',U,'tok_un6ccm.json',['--entry','session.use_env_allocators=1']),
 ('un6_dflt_g8','nllb600m-pruned-un6-ccm-int4-dflt-g8','tok_un6ccm.json',[]),
 ('un6_dflt_g4','nllb600m-pruned-un6-ccm-int4-dflt-g4','tok_un6ccm.json',[]),
 ('un6_dflt_g4_share','nllb600m-pruned-un6-ccm-int4-dflt-g4','tok_un6ccm.json',['--share-emb']),
 ('un6_combo',U,'tok_un6ccm.json',['--share-emb','--arena','off','--prov','arena_extend_strategy=kSameAsRequested']),
 ('main14_base',X,'tok_main14ccm.json',[]),('main14_share',X,'tok_main14ccm.json',['--share-emb']),
 ('main14_combo',X,'tok_main14ccm.json',['--share-emb','--arena','off','--prov','arena_extend_strategy=kSameAsRequested'])]
want=sys.argv[1:] or [j[0] for j in J]
for tag,d,tok,extra in J:
    if tag not in want: continue
    out=f'pb/{tag}.json'
    if os.path.exists(out): continue
    for f in ('encoder_model.onnx','decoder_model_merged.onnx'):
        with open(f'{M}{d}/{f}','rb') as fh:
            while fh.read(8<<20): pass
    info=r.wait_quiet()
    cmd=[r.PY,'purebeam.py','--dir',M+d,'--tok',tok,'--out',out]+extra
    rc=subprocess.run(cmd,env=dict(os.environ,PYTHONPATH=r.ORT128),stdout=open(f'logs/pb-{tag}.log','w'),stderr=subprocess.STDOUT).returncode
    print(tag,rc,flush=True)
