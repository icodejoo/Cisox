// 黄金样本生成器：输出 palette / darken / lighten / mix / 解析 的 C++ 结果，供 Rust 对拍。
// 构建（在 palette-gen 目录）：cl /std:c++17 /EHsc golden_dump.cpp fast_color_lite.cpp palette_generate.cpp
#include <cstdio>
#include <string>
#include <vector>

#include "fast_color_lite.h"
#include "palette_generate.h"

using adqt::theme::FastColorLite;
using adqt::theme::generatePalette;

int main() {
  const std::vector<std::string> bases = {
      "#1677ff", "#f5222d", "#52c41a", "#faad14", "#722ed1", "#13c2c2", "#eb2f96", "#fa8c16",
      "#a0d911", "#fa541c", "#2f54eb", "#fadb14", "#ff4d4f", "#000000", "#ffffff", "#808080"};
  for (const auto& base : bases) {
    for (int dark = 0; dark < 2; ++dark) {
      const auto pal = generatePalette(base, dark != 0, dark ? "#141414" : "");
      std::printf("P %s %s", base.c_str(), dark ? "dark" : "light");
      for (const auto& c : pal) std::printf(" %s", c.c_str());
      std::printf("\n");
    }
    const auto pal2 = generatePalette(base, true, "#000000");
    std::printf("P %s dark000", base.c_str());
    for (const auto& c : pal2) std::printf(" %s", c.c_str());
    std::printf("\n");
  }
  // 无效输入回退
  for (const char* bad : {"#zzzzzz", "", "blue"}) {
    const auto pal = generatePalette(bad, false, "");
    std::printf("PBAD %s", bad[0] ? bad : "<empty>");
    for (const auto& c : pal) std::printf(" %s", c.c_str());
    std::printf("\n");
  }
  // darken / lighten
  for (const auto& base : bases) {
    for (double amt : {4.0, 6.0, 15.0, 8.0, 12.0, 19.0, 26.0}) {
      FastColorLite c(base);
      std::printf("D %s %g %s %s\n", base.c_str(), amt, c.darken(amt).toHexString().c_str(),
                  c.lighten(amt).toHexString().c_str());
    }
  }
  // mix
  for (const auto& a : bases) {
    for (const auto& b : {std::string("#141414"), std::string("#ffffff"), std::string("#1677ff")}) {
      for (double amt : {0.0, 15.0, 50.0, 98.0, 100.0}) {
        std::printf("X %s %s %g %s\n", a.c_str(), b.c_str(), amt,
                    FastColorLite(a).mix(FastColorLite(b), amt).toHexString().c_str());
      }
    }
  }
  // 解析 (valid 标志 + hex + rgb 字符串)
  const std::vector<std::string> inputs = {
      "#1677ff", "#FFF", "#fff8", "#11223344", "#zzzzzz", "#12", "", "abcdefg", "blue", "1677ff",
      "rgb(255, 0, 0)", "rgba(0, 128, 255, 0.5)", "rgb(100%, 0%, 50%)", "rgb(255,0", "rgb(255)",
      "hsl(0,100%,50%)", "RGBA(10,20,30,50%)", "rgb(300,-5,7)", "rgba(1,2,3,0.456)", "rgb(1.5,2.4,3.5)"};
  for (const auto& s : inputs) {
    FastColorLite c(s);
    if (c.isValid())
      std::printf("R [%s] 1 %s %s\n", s.c_str(), c.toHexString().c_str(), c.toRgbString().c_str());
    else
      std::printf("R [%s] 0\n", s.c_str());
  }
  return 0;
}
