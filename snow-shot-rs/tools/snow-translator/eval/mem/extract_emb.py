import onnx, sys, numpy as np
from onnx import numpy_helper
d,out=sys.argv[1],sys.argv[2]
m=onnx.load(f'{d}/encoder_model.onnx',load_external_data=False)
for i in m.graph.initializer:
    if 'embed_tokens' in i.name and len(i.dims)==2 and i.dims[0]>20000 and i.data_type in (2,3):
        np.save(out,numpy_helper.to_array(i)); print(i.name,i.dims,i.data_type)
