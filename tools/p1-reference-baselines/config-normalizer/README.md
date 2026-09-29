# Config Normalizer Baseline

本模块完全重写，以下函数对应 `snow_shot/src/storage/configurationschema.cpp` 中的真实源码，并使用 `nlohmann::json` 等价实现：

1. **`normalizeIntegerRange`** 对应 `snow_shot/src/storage/configurationschema.cpp:1196-1203`
2. **`normalizeTheme`** 对应 `snow_shot/src/storage/configurationschema.cpp:1205-1215`
3. **`normalizeRgbaColor`** 对应 `snow_shot/src/storage/configurationschema.cpp:1438-1449`
4. **`normalizeFilenameFormat`** 对应 `snow_shot/src/storage/configurationschema.cpp:1457-1467`

其余约 16 个 normalizer 函数（涉及快捷键列表/工具栏布局/水印模板等）因逻辑更复杂或依赖项目内部类型未移植，本工具只覆盖上述 4 个作为代表样本。
