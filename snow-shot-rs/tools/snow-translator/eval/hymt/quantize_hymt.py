"""Hy-MT2 单文件 decoder-only ONNX 的 int4 量化：与 NLLB 同一套做法（quantize_onnx.q_int4 的步骤）。

步骤：1) 嵌入 Gather 量化为 int8（quantize_dynamic，逐张量；因模型 >2GB，中间文件用外部数据）；
2) MatMulNBitsQuantizer：bits=4、block 32、对称、RTN，覆盖全部 MatMul（含 lm_head）。
tie_word_embeddings：optimum 导出时 lm_head 权重已是独立的转置副本（[2048, 词表]），所以嵌入表（int8 Gather）与
lm_head（int4 MatMulNBits）是两份各自量化的权重，不共享（二者布局不兼容，见 translation-quantization-benchmark §12.3）。

用法：python quantize_hymt.py --src E:/models/translate-eval/hymt2-1.8b-onnx-fp32 --dst E:/models/translate-eval/hymt2-1.8b-int4
      [--gather-bits 8|0] [--external]
"""
import argparse
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SIDE_SKIP = {".onnx", ".onnx_data"}


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--dst", required=True)
    ap.add_argument("--block", type=int, default=32)
    ap.add_argument("--gather-bits", type=int, choices=[0, 8], default=8, help="嵌入：8=int8 Gather，0=保持 fp32")
    ap.add_argument("--external", action="store_true", help="最终模型也用外部数据文件（单个 .onnx_data）")
    a = ap.parse_args()
    import onnx
    from onnxruntime.quantization import QuantType, quantize_dynamic
    from onnxruntime.quantization import matmul_nbits_quantizer as m
    os.makedirs(a.dst, exist_ok=True)
    src = os.path.join(a.src, "model.onnx")
    mid = src
    if a.gather_bits == 8:
        mid = os.path.join(a.dst, "_g8.onnx")
        print("step1 gather int8", flush=True)
        quantize_dynamic(src, mid, op_types_to_quantize=["Gather"], per_channel=False,
                         weight_type=QuantType.QInt8, use_external_data_format=True)
    print("step2 MatMulNBits int4", flush=True)
    model = onnx.load(mid)
    qz = m.MatMulNBitsQuantizer(model, bits=4, block_size=a.block, is_symmetric=True, op_types_to_quantize=("MatMul",),
                                algo_config=m.RTNWeightOnlyQuantConfig())
    qz.process()
    out = os.path.join(a.dst, "model.onnx")
    if a.external:
        qz.model.save_model_to_file(out, use_external_data_format=True)
    else:
        qz.model.save_model_to_file(out, use_external_data_format=False)
    for fn in os.listdir(a.dst):
        if fn.startswith("_g8"):
            os.remove(os.path.join(a.dst, fn))
    for fn in os.listdir(a.src):
        p = os.path.join(a.src, fn)
        if os.path.isfile(p) and os.path.splitext(fn)[1] not in SIDE_SKIP:
            shutil.copy2(p, os.path.join(a.dst, fn))
    for fn in sorted(os.listdir(a.dst)):
        print(fn, round(os.path.getsize(os.path.join(a.dst, fn)) / 1048576, 1), "MiB")


if __name__ == "__main__":
    main()
