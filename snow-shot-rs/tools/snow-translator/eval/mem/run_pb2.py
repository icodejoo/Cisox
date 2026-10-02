import sys, os, subprocess, json
sys.path.insert(0,'E:/workspaces/Cisox/snow-shot-rs/tools/snow-translator/eval')
import run_quant_matrix as r
M='E:/models/translate-eval/'
U='nllb600m-pruned-un6-ccm-int4'; X='nllb600m-pruned-main14-ccm-int4'
G='nllb600m-pruned-main14-ccm-int4-dflt-g4'; T='tok_main14ccm.json'; SAR=['--prov','arena_extend_strategy=kSameAsRequested']
J=[('main14_beam2',X,T,['--beams','2']),('main14_share_beam2',X,T,['--share-emb','--beams','2']),
 ('main14_dflt_g4',G,T,[]),('main14_dflt_g4_beam2',G,T,['--beams','2']),
 ('main14_share_sar',X,T,['--share-emb']+SAR),('main14_best',X,T,['--share-emb','--beams','2']+SAR),
 ('main14_ext','_exp/main14-ext',T,[])]
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
