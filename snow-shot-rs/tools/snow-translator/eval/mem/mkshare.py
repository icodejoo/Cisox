"""找出 encoder 与 decoder_model_merged 里内容完全相同的大 initializer（>1MB），导出 npy 与映射 json。用法: mkshare.py 模型目录 输出前缀"""
import onnx, sys, hashlib, json, numpy as np
from onnx import numpy_helper
d,pre=sys.argv[1],sys.argv[2]
def big(path):
    m=onnx.load(path,load_external_data=False); r={}
    for i in m.graph.initializer:
        a=numpy_helper.to_array(i)
        if a.nbytes>1<<20: r[i.name]=(hashlib.md5(a.tobytes()).hexdigest(),a)
    return r
e=big(f'{d}/encoder_model.onnx'); dd=big(f'{d}/decoder_model_merged.onnx')
byh={h:(n,a) for n,(h,a) in e.items()}; mp=[]
for n,(h,a) in dd.items():
    if h in byh:
        en=byh[h][0]; f=f'{pre}_{len(mp)}.npy'; np.save(f,a); mp.append({'enc':en,'dec':n,'file':f,'mib':round(a.nbytes/1048576,1)})
json.dump(mp,open(pre+'.json','w'),indent=1); print(mp)
