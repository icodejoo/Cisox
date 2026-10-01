"""把 optimum 导出的 fp32 翻译 ONNX（encoder + decoder_model_merged）量化为 int8 / int4。

int8：onnxruntime `quantize_dynamic`（MatMul 权重 per-channel int8 + 动态激活量化，Gather 嵌入 int8）。
int4：`MatMulNBitsQuantizer`（block_size=32、对称、RTN，MatMul 含 lm_head）+ Gather 嵌入 int8（quantize_dynamic）。
用法示例：
    python quantize_onnx.py --src E:/models/translate-eval/_onnx-fp32/x --dst E:/models/translate-eval/x-int8 --mode int8
"""
import argparse
import os
import shutil
import sys
import tempfile

ENCODER = "encoder_model.onnx"
DECODERS = ("decoder_model.onnx", "decoder_with_past_model.onnx")
MERGED = "decoder_model_merged.onnx"
SIDE_SKIP = {".onnx", ".onnx_data"}


def q_int8(src, dst, gather=True):
    """动态量化为 int8。

    参数：src/dst 模型路径；gather 是否同时量化嵌入 Gather。
    """
    from onnxruntime.quantization import QuantType, quantize_dynamic
    ops = ["MatMul"] + (["Gather"] if gather else [])
    quantize_dynamic(src, dst, op_types_to_quantize=ops, per_channel=True, reduce_range=False,
                     weight_type=QuantType.QInt8, use_external_data_format=False)


def q_int4(src, dst, block=32, algo="rtn", gather_bits=8, bits=4):
    """MatMulNBits int4 量化，嵌入按 gather_bits 处理（8=int8，4=GatherBlockQuantized，0=保持 fp32）。

    参数：src/dst 模型路径；block 块大小；algo rtn|hqq；gather_bits 见上；bits 权重位数（8 即"仅权重 int8"，走 DEFAULT 算法）。
    """
    import onnx
    from onnxruntime.quantization import matmul_nbits_quantizer as m
    ops = ("MatMul", "Gather") if gather_bits == 4 else ("MatMul",)
    if bits == 8:  # RTN 路径在 ORT 里写死 4 位，8 位权重只能走 DEFAULT 算法
        cfg = None
    elif algo == "rtn":
        cfg = m.RTNWeightOnlyQuantConfig(ops, bits=4) if "bits" in m.RTNWeightOnlyQuantConfig.__init__.__code__.co_varnames else m.RTNWeightOnlyQuantConfig()
    else:
        cfg = m.HQQWeightOnlyQuantConfig(block_size=block, bits=4)
    # 先量化嵌入 Gather（int8），再做 MatMulNBits：反过来时第二步的形状推断找不到 com.microsoft 域
    mid = src
    if gather_bits == 8:
        mid = dst + ".g8.onnx"
        q_int8_gather_only(src, mid)
    model = onnx.load(mid)
    qz = m.MatMulNBitsQuantizer(model, bits=bits, block_size=block, is_symmetric=True, op_types_to_quantize=ops,
                                algo_config=cfg)
    qz.process()
    qz.model.save_model_to_file(dst, use_external_data_format=False)
    if mid != src:
        os.remove(mid)


def q_int8_gather_only(src, dst):
    """只把 Gather 嵌入量化为 int8（保留已有 MatMulNBits）。"""
    from onnxruntime.quantization import QuantType, quantize_dynamic
    quantize_dynamic(src, dst, op_types_to_quantize=["Gather"], per_channel=False,
                     weight_type=QuantType.QInt8, use_external_data_format=False)


def copy_sidecars(src_dir, dst_dir):
    """复制配置/分词器等非 onnx 文件。"""
    for fn in os.listdir(src_dir):
        p = os.path.join(src_dir, fn)
        if os.path.isfile(p) and os.path.splitext(fn)[1] not in SIDE_SKIP:
            shutil.copy2(p, os.path.join(dst_dir, fn))


def link_local(path, local_dir):
    """把源 onnx（及其 .onnx_data）硬链接到私有目录：量化器会在源文件旁写 *-inferred.onnx，并发任务共享源目录会互相踩。"""
    os.makedirs(local_dir, exist_ok=True)
    out = os.path.join(local_dir, os.path.basename(path))
    external = os.path.exists(path + "_data")  # onnx 拒绝读多硬链接的外部数据文件，这类模型改为复制
    for suffix in ("", "_data"):
        src = path + suffix
        if os.path.exists(src):
            dst = os.path.join(local_dir, os.path.basename(src))
            if not os.path.exists(dst):
                try:
                    if external:
                        raise OSError
                    os.link(src, dst)
                except OSError:
                    shutil.copy2(src, dst)
    return out


def quantize_file(mode, s, d, a):
    """按模式量化单个 onnx 文件（先链接到 dst/_src 私有目录）。"""
    s = link_local(s, os.path.join(a.dst, "_src"))
    if mode == "int8":
        q_int8(s, d)
    else:
        q_int4(s, d, a.block, a.algo, a.gather_bits, bits=8 if mode == "int8wo" else 4)


def main():
    """命令行入口：量化 encoder，分别量化两份 decoder 后再合并（merged 图的子图引用外层权重，量化器处理不到）。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--dst", required=True)
    ap.add_argument("--mode", choices=["int8", "int4", "int8wo"], required=True)
    ap.add_argument("--algo", choices=["rtn", "hqq"], default="rtn")
    ap.add_argument("--block", type=int, default=32)
    ap.add_argument("--gather-bits", type=int, choices=[0, 4, 8], default=8)
    a = ap.parse_args()
    from optimum.onnx.graph_transformations import merge_decoders
    os.makedirs(a.dst, exist_ok=True)
    tmp = os.path.join(a.dst, "_tmp")
    os.makedirs(tmp, exist_ok=True)
    print("quantize", ENCODER, flush=True)
    quantize_file(a.mode, os.path.join(a.src, ENCODER), os.path.join(a.dst, ENCODER), a)
    parts = []
    for fn in DECODERS:
        print("quantize", fn, flush=True)
        t = os.path.join(tmp, fn)
        quantize_file(a.mode, os.path.join(a.src, fn), t, a)
        parts.append(t)
    print("merge decoders", flush=True)
    import onnx
    real_check = onnx.checker.check_model
    onnx.checker.check_model = lambda *args, **kw: None  # merge_decoders 内部校验不认 com.microsoft 域，合并后再补域
    try:
        raw = os.path.join(tmp, "merged_raw.onnx")
        merge_decoders(parts[0], parts[1], strict=False, save_path=raw)
        merged = onnx.load(raw)
    finally:
        onnx.checker.check_model = real_check
    have = {o.domain for o in merged.opset_import}
    for o in onnx.load(parts[0], load_external_data=False).opset_import:  # 补上 com.microsoft 等自定义域
        if o.domain not in have:
            merged.opset_import.append(o)
    onnx.save(merged, os.path.join(a.dst, MERGED))
    shutil.rmtree(tmp)
    shutil.rmtree(os.path.join(a.dst, "_src"), ignore_errors=True)
    for fn in (ENCODER, MERGED):
        print(fn, round(os.path.getsize(os.path.join(a.dst, fn)) / 1048576, 1), "MiB", flush=True)
    copy_sidecars(a.src, a.dst)


if __name__ == "__main__":
    main()
