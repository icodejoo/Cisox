# AVX2 滤镜模块 (AVX2 Filters)

本目录包含了基于 AVX2 指令集优化的图像处理滤镜参考实现。

## 功能特性

- **高性能滤镜处理**：针对灰度 (`grayscale`) 和反相 (`invert`) 等常用滤镜，提供了基于 AVX2 向量化指令集的实现。
- **灵活的区域选择**：支持全图像处理、指定矩形区域处理以及基于遮罩 (Alpha mask) 的选区处理。
- **图像缩放与复制**：提供了 4 抽头下采样 (`downsampleFourTapAvx2`) 以及快速的行复制操作 (`copyRowsAvx2`)。
- **平滑插值**：包含 `interpolateAndBlendConstantAvx2` 等支持高斯模糊等特效前置计算的高效混合函数。

## 模块说明

- `snow_canvas_filter_avx2.h` / `.cpp`: 核心 AVX2 滤镜的实现。
- `snow_canvas_pen_mask_avx2.h` / `.cpp`: 基于 AVX2 的画笔/遮罩渲染逻辑。

## 编译与测试

本模块依赖于支持 AVX2 的 CPU 架构和相应的编译器选项。请通过父目录的 CMake 构建系统进行编译。

## 注意事项

- 调用 AVX2 相关函数之前，建议在运行时检测当前 CPU 是否支持 AVX2 指令集。
- 图像的行跨度（stride）对性能有显著影响，内部循环针对缓存行做了初步优化。
