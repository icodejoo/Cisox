"""会话选项对内存的影响：对 main14-ccm-int4 / int8 跑性能遍（2 语向 x 30 句，默认线程），每个变体单独进程，运行前确认安静。"""
import json, os, subprocess, sys
sys.path.insert(0, 'E:/workspaces/Cisox/snow-shot-rs/tools/snow-translator/eval')
import run_quant_matrix as r
VARIANTS = {
  'base':      dict(cfg='default', extra=[]),
  'basicopt':  dict(cfg='default', extra=['--opt-level', 'basic']),
  'noopt':     dict(cfg='default', extra=['--opt-level', 'disable']),
  'tight+basicopt': dict(cfg='tight', extra=['--opt-level', 'basic']),
  'tight+noopt': dict(cfg='tight', extra=['--opt-level', 'disable']),
}
MODELS = sys.argv[1:] or ['nllb600m-pruned-main14-ccm-int4', 'nllb600m-pruned-main14-ccm-int8']
for name in MODELS:
    kind, pruned, _ = r.JOBS[name]
    for vn, v in VARIANTS.items():
        out = f'{r.QUANT}/metrics/exp-{name}.{vn}.json'
        if os.path.exists(out): continue
        r.warm_cache(name); info = r.wait_quiet()
        cmd = [r.PY, f'{r.HERE}/eval_quant.py', '--dir', f'{r.MODELS}/{name}', '--kind', kind, '--pruned-dir', pruned, '--name', name,
               '--config', v['cfg'], '--pairs', 'perf', '--hyp-root', f'{r.QUANT}/perf-hyp/exp-{name}-{vn}', '--out-json', out] + v['extra']
        rc = subprocess.run(cmd, env=dict(os.environ, PYTHONPATH=r.ORT128), stdout=open(f'{r.QUANT}/logs/exp-{name}.{vn}.log', 'w'), stderr=subprocess.STDOUT).returncode
        print(name, vn, rc, flush=True)
