"""把单文件 int4 ONNX 改存成外部数据（model.onnx + model.onnx_data），供 ORT 内存映射；不改任何权重。

用法：python make_ext.py --src <含 model.onnx 的目录> --dst <输出目录>
"""
import argparse
import os
import shutil

import onnx


def main():
    """入口：读入、存外部数据、复制 tokenizer 等旁文件。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--dst", required=True)
    a = ap.parse_args()
    os.makedirs(a.dst, exist_ok=True)
    m = onnx.load(os.path.join(a.src, "model.onnx"))
    onnx.save_model(m, os.path.join(a.dst, "model.onnx"), save_as_external_data=True, all_tensors_to_one_file=True,
                    location="model.onnx_data", size_threshold=1024)
    for fn in os.listdir(a.src):
        if os.path.splitext(fn)[1] not in (".onnx", ".onnx_data") and not fn.startswith("_"):
            shutil.copy2(os.path.join(a.src, fn), os.path.join(a.dst, fn))
    for fn in sorted(os.listdir(a.dst)):
        print(fn, round(os.path.getsize(os.path.join(a.dst, fn)) / 1048576, 1), "MiB")


if __name__ == "__main__":
    main()
