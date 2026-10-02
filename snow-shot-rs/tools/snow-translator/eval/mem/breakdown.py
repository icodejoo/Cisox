import onnx, sys, collections
from onnx import numpy_helper
d=sys.argv[1]
tot=0
for part in ['encoder_model.onnx','decoder_model_merged.onnx']:
    m=onnx.load(f'{d}/{part}',load_external_data=False)
    inits={}
    def collect(g):
        for i in g.initializer: inits[i.name]=i
        for n in g.node:
            for a in n.attribute:
                if a.g.node or a.g.initializer: collect(a.g)
    collect(m.graph)
    use={}
    def walk(g):
        for n in g.node:
            for k,i in enumerate(n.input):
                if i in inits: use.setdefault(i,[]).append((n.op_type,k,n.name))
            for a in n.attribute:
                if a.g.node: walk(a.g)
    walk(m.graph)
    cat=collections.Counter(); cnt=collections.Counter()
    for nm,t in inits.items():
        size=len(t.raw_data) if t.raw_data else numpy_helper.to_array(t).nbytes
        u=use.get(nm,[('unused',0,'')])
        ops={x[0] for x in u}
        if 'Gather' in ops and max(t.dims or [0])>20000: c='嵌入 Gather 权重'
        elif 'DequantizeLinear' in ops and max(t.dims or [0])>20000: c='嵌入 Gather 权重(scale/zp)'
        elif 'MatMulNBits' in ops:
            big=max(t.dims or [0])
            c='MatMulNBits 权重/scales: lm_head' if any('lm_head' in x[2] for x in u) else 'MatMulNBits 权重/scales: 各层'
        elif 'MatMul' in ops: c='MatMul fp32 权重'
        else: c='其它(LayerNorm/bias/位置编码/常量)'
        cat[c]+=size; cnt[c]+=1
        if size>5e6 and c.startswith(('其它','嵌入','MatMul fp32')): print('  大项',part,nm,list(t.dims),round(size/1048576,1),sorted(ops))
    print(part, round(sum(cat.values())/1048576,1),'MiB')
    for c,v in cat.most_common(): print('  ',c,cnt[c],round(v/1048576,1))
    tot+=sum(cat.values())
print('total initializer MiB',round(tot/1048576,1))
