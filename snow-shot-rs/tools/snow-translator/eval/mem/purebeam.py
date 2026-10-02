"""纯 ORT + numpy 束搜索（不导入 torch），模拟 Rust worker 的 encoder + merged decoder(KV cache) 流程，测内存/延迟/译文 id。
用法: purebeam.py --dir 模型目录 --tok tok.json --out out.json [--beams 4] [--share-emb] [--arena off] ..."""
import sys, os, json, time, argparse, threading
import ctypes, ctypes.wintypes as wt
class C(ctypes.Structure):
    _fields_=[("cb",wt.DWORD),("pf",wt.DWORD),("peak",ctypes.c_size_t),("ws",ctypes.c_size_t),("a",ctypes.c_size_t),("b",ctypes.c_size_t),("c",ctypes.c_size_t),("d",ctypes.c_size_t),("pagefile",ctypes.c_size_t),("pp",ctypes.c_size_t)]
k=ctypes.WinDLL("kernel32"); k.GetCurrentProcess.restype=wt.HANDLE
ps=ctypes.WinDLL("psapi"); ps.GetProcessMemoryInfo.argtypes=[wt.HANDLE,ctypes.POINTER(C),wt.DWORD]
def ws():
    c=C(); c.cb=ctypes.sizeof(c); ps.GetProcessMemoryInfo(k.GetCurrentProcess(),ctypes.byref(c),c.cb); ws.commit=c.pagefile/1048576; return c.ws/1048576, c.peak/1048576
import numpy as np, onnxruntime as ort, onnx
from onnx import numpy_helper
ap=argparse.ArgumentParser()
ap.add_argument('--dir',required=True); ap.add_argument('--tok',required=True); ap.add_argument('--out',required=True)
ap.add_argument('--beams',type=int,default=4); ap.add_argument('--max-new',type=int,default=256); ap.add_argument('--lp',type=float,default=1.0)
ap.add_argument('--share-emb',action='store_true'); ap.add_argument('--arena',default='on'); ap.add_argument('--threads',type=int,default=0)
ap.add_argument('--entry',action='append',default=[]); ap.add_argument('--prov',action='append',default=[])
ap.add_argument('--pairs',default='zho_Hans-eng_Latn,eng_Latn-fra_Latn,eng_Latn-zho_Hans'); ap.add_argument('--limit',type=int,default=30)
ap.add_argument('--enc-file',default='encoder_model.onnx'); ap.add_argument('--dec-file',default='decoder_model_merged.onnx')
a=ap.parse_args()
base,_=ws(); base_commit=ws.commit
def so():
    s=ort.SessionOptions()
    if a.arena=='off': s.enable_cpu_mem_arena=False; s.enable_mem_pattern=False
    if a.threads: s.intra_op_num_threads=a.threads
    for kv in a.entry:
        kk,v=kv.split('=',1); s.add_session_config_entry(kk,v)
    return s
provs=[('CPUExecutionProvider',dict(kv.split('=',1) for kv in a.prov))] if a.prov else ['CPUExecutionProvider']
so_e,so_d=so(),so()
keep=[]
if a.share_emb:  # 由 mkshare.py 预先导出的"encoder 与 decoder 内容相同的大 initializer"，加载 npy 后共享同一份内存
    mp=json.load(open('share_main14.json' if 'main14' in a.dir else ('share_un6_g4.json' if 'g4' in a.dir else 'share_un6.json')))
    for e in mp:
        arr=np.load(e['file']); ov=ort.OrtValue.ortvalue_from_numpy(arr); keep.append((ov,arr))
        so_e.add_initializer(e['enc'],ov); so_d.add_initializer(e['dec'],ov)
        print('shared',e['enc'],e['dec'],arr.shape,arr.dtype,flush=True)
t0=time.time()
enc=ort.InferenceSession(f'{a.dir}/{a.enc_file}',so_e,providers=provs); w1,_=ws()
dec=ort.InferenceSession(f'{a.dir}/{a.dec_file}',so_d,providers=provs); w2,pk=ws(); load_commit=ws.commit-base_commit
load_s=time.time()-t0
print('load enc',round(w1-base),'dec+enc',round(w2-base),'peak',round(pk-base),'s',round(load_s,1),flush=True)
pnames=[i.name for i in dec.get_inputs() if i.name.startswith('past_key_values.')]
class S(threading.Thread):
    def __init__(s): super().__init__(daemon=True); s.m=0; s.f=False
    def run(s):
        while not s.f: s.m=max(s.m,ws()[0]); time.sleep(0.02)
smp=S(); smp.start()
def lsm(x):
    x=x-x.max(-1,keepdims=True); return x-np.log(np.exp(x).sum(-1,keepdims=True))
def translate(src,forced,W=a.beams,start=2,eos=2):
    ids=np.array([src],dtype=np.int64); mask=np.ones_like(ids)
    h=enc.run(None,{'input_ids':ids,'attention_mask':mask})[0]
    h=np.repeat(h,W,0); em=np.repeat(mask,W,0)
    past={n:np.zeros((W,16,0,64),np.float32) for n in pnames}
    scores=np.zeros(W); scores[1:]=-1e9
    seqs=[[] for _ in range(W)]; fin=[]; cur=np.full((W,1),start,np.int64); done=False
    for step in range(a.max_new):
        feed={'input_ids':cur,'encoder_attention_mask':em,'encoder_hidden_states':h,'use_cache_branch':np.array([step>0])}
        feed.update(past)
        outs=dec.run(None,feed); od={o.name:v for o,v in zip(dec.get_outputs(),outs)}
        lg=lsm(od['logits'][:,-1].astype(np.float32)); V=lg.shape[-1]
        if step==0:
            f=np.full_like(lg,-1e9); f[:,forced]=0; lg=f
        cand=(scores[:,None]+lg).reshape(-1)
        top=np.argpartition(-cand,2*W)[:2*W]; top=top[np.argsort(-cand[top])]
        ns,nt,np_=[],[],[]
        for r,idx in enumerate(top):
            par,tok=divmod(int(idx),V); sc=cand[idx]
            if tok==eos:
                if r<W and sc>-1e8: fin.append((sc/((len(seqs[par])+1)**a.lp),seqs[par]+[tok]))
            else: ns.append(sc); nt.append(tok); np_.append(par)
            if len(ns)==W: break
        if len(fin)>=W:
            fin.sort(key=lambda x:-x[0]); fin=fin[:W]
            if fin[-1][0]>=max(ns)/((step+1)**a.lp): done=True
        if done or not ns: break
        scores=np.array(ns); seqs=[seqs[p]+[t] for p,t in zip(np_,nt)]; cur=np.array(nt,np.int64)[:,None]
        for n in pnames:
            pr=n.replace('past_key_values.','present.'); v=od[pr]
            if '.encoder.' in n:
                if step==0: past[n]=v
            else: past[n]=v[np_]
    if not done:
        for sc,sq in zip(scores,seqs): fin.append((sc/((len(sq)+1)**a.lp),sq))
    return max(fin,key=lambda x:x[0])[1]
tk=json.load(open(a.tok)); res={}; lats=[]
for p in a.pairs.split(','):
    res[p]=[]
    for s in tk[p]['src_ids'][:a.limit]:
        t=time.time(); res[p].append(translate(s,tk[p]['forced'])); lats.append(time.time()-t)
smp.f=True; time.sleep(0.05); wend,pkend=ws()
m={'base':round(base),'load_enc_delta':round(w1-base),'load_delta':round(w2-base),'peak_load_delta':round(pk-base),'load_s':round(load_s,2),'translate_peak_delta':round(smp.m-base),'end_delta':round(wend-base),'load_private':round(load_commit),'end_private':round(ws.commit-base_commit),'lat_mean':round(float(np.mean(lats)),3),'lat_p50':round(float(np.median(lats)),3),'args':vars(a)}
print(json.dumps(m)); json.dump({'ids':res,'metrics':m},open(a.out,'w'))
