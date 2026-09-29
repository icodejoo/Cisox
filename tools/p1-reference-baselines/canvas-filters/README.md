# canvas-filters 黄金样本（ADR-10 / 约定 7）

直接编译**真实的** C++ 滤镜内核，产出供 Rust `snow-canvas-filters` 逐字节对拍的黄金样本。

- 被编译的源码（不改动）：`snow_draw_engine_qt/src/rendering/` 下的
  `snow_canvas_filter_render.cpp`、`snow_canvas_filter_avx2.cpp`、`snow_canvas_pen_mask_avx2.cpp`。
- `qt_shim/`：最小 Qt 兼容层（QImage 写时复制、QRect、QRegion 简化版等），让上述文件无需 Qt 即可编译。
- `diagnostics_stub.cpp`：诊断开关桩。
- `golden_main.cpp`：读 `cases.txt`，把每条用例的输出写入 `golden.bin`；`bench` 子命令测 4K 单线程耗时。
- `gen_cases.ps1` → `cases.txt`（1193 条：奇数尺寸、小于 SIMD 宽度的图、贴边/越界矩形、遮罩偏移/不足、极端参数）。
- `golden.bin`：输出记录 `[u32 名称长][名称][u32 数据长][数据]`；数据长为 `0xFFFFFFFF` 表示“与对应 `_fs0` 用例相同”。
  输入不入库：图像由固定种子 xorshift32 生成，Rust 测试用同一算法复现。

## 重新生成

```powershell
powershell -File build.ps1 -Out <输出目录>      # 需要 VS2022 BuildTools（可用 -VcVars 指定 vcvars64.bat）
powershell -File gen_cases.ps1
<输出目录>\golden_main.exe cases.txt golden.bin  # 会打印 C++ 标量与 AVX2 不一致的用例数（当前为 0）
<输出目录>\golden_main.exe bench                 # 4K 耗时
```

`fs0` = 允许 AVX2（forceScalar=0），`fs1` = 强制标量（forceScalar=1）。
Rust 侧：`cargo test -p snow-canvas-filters --release --test golden`。
