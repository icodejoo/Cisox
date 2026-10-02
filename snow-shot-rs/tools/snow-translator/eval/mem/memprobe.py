"""仅用 onnxruntime + numpy 的内存探针（不导入 torch）：分别建 encoder / decoder 会话并读工作集。"""
import sys, os, json, time, argparse
sys.path.insert(0,'E:/workspaces/Cisox/snow-shot-rs/tools/snow-translator/eval')
import ctypes, ctypes.wintypes as wt
class C(ctypes.Structure):
    _fields_=[("cb",wt.DWORD),("pf",wt.DWORD),("peak",ctypes.c_size_t),("ws",ctypes.c_size_t),("a",ctypes.c_size_t),("b",ctypes.c_size_t),("c",ctypes.c_size_t),("d",ctypes.c_size_t),("pagefile",ctypes.c_size_t),("pp",ctypes.c_size_t)]
k=ctypes.WinDLL("kernel32"); k.GetCurrentProcess.restype=wt.HANDLE
ps=ctypes.WinDLL("psapi"); ps.GetProcessMemoryInfo.argtypes=[wt.HANDLE,ctypes.POINTER(C),wt.DWORD]
def ws():
    c=C(); c.cb=ctypes.sizeof(c); ps.GetProcessMemoryInfo(k.GetCurrentProcess(),ctypes.byref(c),c.cb); return c.ws/1048576, c.peak/1048576
import numpy as np, onnxruntime as ort
ap=argparse.ArgumentParser(); ap.add_argument('--dir',required=True); ap.add_argument('--entry',action='append',default=[]); ap.add_argument('--arena',default='on'); ap.add_argument('--parts',default='enc,dec'); ap.add_argument('--opt',default='all')
a=ap.parse_args()
def so():
    s=ort.SessionOptions()
    if a.arena=='off': s.enable_cpu_mem_arena=False; s.enable_mem_pattern=False
    s.graph_optimization_level={'all':ort.GraphOptimizationLevel.ORT_ENABLE_ALL,'basic':ort.GraphOptimizationLevel.ORT_ENABLE_BASIC,'disable':ort.GraphOptimizationLevel.ORT_DISABLE_ALL}[a.opt]
    for kv in a.entry:
        kk,v=kv.split('=',1); s.add_session_config_entry(kk,v)
    return s
b,_=ws(); print('baseline',round(b),flush=True)
res={}
sess={}
for p,f in (('enc','encoder_model.onnx'),('dec','decoder_model_merged.onnx')):
    if p not in a.parts: continue
    t=time.time(); sess[p]=ort.InferenceSession(f'{a.dir}/{f}',so(),providers=['CPUExecutionProvider'])
    w,pk=ws(); res[p]=(round(w-b),round(pk-b)); print(p,'load_delta',round(w-b),'peak_delta',round(pk-b),'t',round(time.time()-t,1),flush=True)
print(json.dumps(res))
