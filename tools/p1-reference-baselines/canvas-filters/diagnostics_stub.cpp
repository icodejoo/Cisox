// snow_canvas_render_diagnostics 的桩实现：黄金样本工具不需要诊断计时。
#include "snow_canvas_render_diagnostics.h"

namespace snow_canvas_render_diagnostics {
void setEnabled(bool) {}
bool isEnabled() { return false; }
} // namespace snow_canvas_render_diagnostics
