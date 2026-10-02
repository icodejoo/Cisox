"""解码 purebeam 输出 id 并打分；与基线 json 比逐句一致率。用法: posttok.py 裁剪HF目录 out.json [基线out.json]"""
import sys, json
sys.path.insert(0,'E:/workspaces/Cisox/snow-shot-rs/tools/snow-translator/eval')
from eval_quant import read_flores, ORIG_DIR
from prune_nllb_vocab import PrunedNllbTokenizer
from chrf import corpus_chrf
d,f=sys.argv[1],sys.argv[2]; bf=sys.argv[3] if len(sys.argv)>3 else None
tok=PrunedNllbTokenizer(ORIG_DIR,d,mode="remap")
def dec(j): return {p:[tok.decode([ids])[0] for ids in v] for p,v in json.load(open(j))['ids'].items()}
h=dec(f); b=dec(bf) if bf else None
out={}
for p,hy in h.items():
    ref=read_flores(p.split('-')[1],len(hy))
    tgt=p.split('-')[1]; cjk=tgt=='zho_Hans'
    sc=corpus_chrf(hy,ref,word_order=0) if cjk else corpus_chrf(hy,ref)
    same=sum(x==y for x,y in zip(hy,b[p])) if b else None
    out[p]=(round(sc,2),same,len(hy)); 
print(json.dumps(out))
