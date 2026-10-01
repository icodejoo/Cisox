"""把 HF 的 Marian / M2M100 翻译模型导出为 ONNX（optimum，encoder + 带 KV cache 的 decoder + merged）。

为什么要补丁：Python 3.14 起 functools.partial 变成方法描述符，optimum 的
`NormalizedConfig.with_args`（返回 partial 并存为类属性）会被绑定 self，导出时报
"got multiple values for argument 'allow_new'"。这里在导入导出器之前把返回值包成 staticmethod 即可。

用法示例：
    python export_quant_onnx.py --model E:/models/x --out E:/models/translate-eval/_onnx-fp32/x
"""
import argparse
import functools


def patch_normalized_config():
    """修补 optimum NormalizedConfig.with_args，使其在 Python 3.14 下作为类属性不被绑定。"""
    from optimum.utils import normalized_config as nc

    def with_args(cls, allow_new=False, **kwargs):
        return staticmethod(functools.partial(cls, allow_new=allow_new, **kwargs))

    nc.NormalizedConfig.with_args = classmethod(with_args)


def main():
    """命令行入口：导出到目录。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--opset", type=int, default=17)
    a = ap.parse_args()
    patch_normalized_config()
    from optimum.exporters.onnx import main_export
    main_export(model_name_or_path=a.model, output=a.out, task="text2text-generation-with-past",
                opset=a.opset, do_validation=False)


if __name__ == "__main__":
    main()
