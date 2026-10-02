"""int8 动态量化变体：只量化 MatMul，保留嵌入 Gather 为 fp32（排查 zh-en 整档 int8 输出退化）。

复用 quantize_onnx.py 的整条流程（encoder、两份 decoder 分别量化后合并），仅把 q_int8 的 gather 默认值改成 False。
用法：python quantize_int8_nogather.py --src <fp32 目录> --dst <输出目录> --mode int8
"""
import functools
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import quantize_onnx  # noqa: E402

quantize_onnx.q_int8 = functools.partial(quantize_onnx.q_int8, gather=False)
quantize_onnx.main()
