"""预分词：把评测句子编码成裁剪词表 id 存 json，供不依赖 torch 的纯 ORT 束搜索使用。用法: pretok.py 裁剪HF目录 输出json"""
import sys, json
sys.path.insert(0,'E:/workspaces/Cisox/snow-shot-rs/tools/snow-translator/eval')
from eval_quant import read_flores, ORIG_DIR
from prune_nllb_vocab import PrunedNllbTokenizer
d,out=sys.argv[1],sys.argv[2]
tok=PrunedNllbTokenizer(ORIG_DIR,d,mode="remap")
res={}
for src,tgt in [("zho_Hans","eng_Latn"),("eng_Latn","fra_Latn"),("eng_Latn","zho_Hans")]:
    ids=[tok.encode([s],src)["input_ids"][0].tolist() for s in read_flores(src,30)]
    res[f"{src}-{tgt}"]={"src_ids":ids,"forced":tok.lang_id(tgt)}
json.dump(res,open(out,'w'))
print({k:len(v['src_ids']) for k,v in res.items()}, 'ex', res['zho_Hans-eng_Latn']['src_ids'][0][:6])
