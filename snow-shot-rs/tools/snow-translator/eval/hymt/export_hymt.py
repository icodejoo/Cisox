"""把 Hy-MT2-1.8B（hunyuan_v1_dense，Llama 式解码器 + QK-Norm + GQA）导出为带 KV cache 的 fp32 ONNX。

optimum 2.1 没有该架构的导出配置：这里复用 Qwen3OnnxConfig（同为 GQA + QK-Norm + 显式 head_dim）注册一份，
并沿用 export_quant_onnx 的 Python 3.14 补丁。导出任务 text-generation-with-past，产物是单个
model.onnx（带 position_ids / past_key_values.N.key|value 输入）。

用法：python export_hymt.py --model E:/models/translate-eval/hy-mt2-1.8b --out E:/models/translate-eval/hymt2-1.8b-onnx-fp32
"""
import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from export_quant_onnx import patch_normalized_config  # noqa: E402


def register_hunyuan():
    """向 optimum 注册 hunyuan_v1_dense 的 ONNX 导出配置（继承 Qwen3OnnxConfig）。"""
    from optimum.exporters.onnx.model_configs import COMMON_TEXT_GENERATION_TASKS, Qwen3OnnxConfig, register_tasks_manager_onnx

    @register_tasks_manager_onnx("hunyuan_v1_dense", *COMMON_TEXT_GENERATION_TASKS)
    class HunyuanDenseOnnxConfig(Qwen3OnnxConfig):
        """Hy-MT2 的导出配置。"""

    return HunyuanDenseOnnxConfig


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--opset", type=int, default=17)
    a = ap.parse_args()
    patch_normalized_config()
    register_hunyuan()
    from optimum.exporters.onnx import main_export
    main_export(model_name_or_path=a.model, output=a.out, task="text-generation-with-past", opset=a.opset,
                dtype="fp32", do_validation=False, no_post_process=True, library_name="transformers")


if __name__ == "__main__":
    main()
