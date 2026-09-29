# P1 Reference Baselines

本目录 (`tools/p1-reference-baselines`) 包含了多个为项目核心功能提供参考实现和基准验证的独立模块。每个子目录均是一个独立的 CMake 工程，可用于快速验证算法可行性、提供独立的测试桩 (test double) 或是为 C++ 与其他语言（如 Rust）的交互提供 C++ 参考。

## 包含的模块

1. **avx2-filters**
   - **功能**: 基于 AVX2 指令集的高性能图像滤镜参考实现。
   - **内容**: 提供了灰度 (grayscale)、反相 (invert)、遮罩选区合成以及下采样等核心算法的 SIMD 优化版本。该模块已有完整的文档说明。
   
2. **palette-gen**
   - **功能**: 用于 Ant Design 色板生成算法的 C++ 移植参考实现。
   - **内容**: 修复了越界和无效颜色输入时的崩溃 Bug。支持从主色调 (base color) 生成 Light 和 Dark 主题下的 10 个梯度色阶，为 UI 的动态主题提供算法基准。

3. **selection-codec**
   - **功能**: 选区和窗口几何信息 JSON 序列化与反序列化的基准实现。
   - **内容**: 基于 nlohmann/json 提供了 `PersistedSelection` 和 `WindowGeometry` 结构的无缝互相转换参考，严格校验各种参数边界并支持复杂的多边形选区数据编码。

4. **config-normalizer**
   - **功能**: 用户配置解析及规范化 (Normalization) 参考工具。
   - **内容**: 通过解析非法的或不合规的 JSON 配置（如超范围的值、不支持的格式），将其恢复回系统支持的安全默认值（例如：自动限制质量 `quality` 的上限、重置 `theme` 等），确保程序健壮性。

## 编译方法

每个子模块包含独立的 `CMakeLists.txt`。可以使用标准的 CMake 流程编译：

```bash
cd <module-dir>
mkdir build && cd build
cmake ..
cmake --build . --config Debug
```
