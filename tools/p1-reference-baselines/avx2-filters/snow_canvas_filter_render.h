#pragma once
#include <cstdint>
#include <cstddef>
#include <algorithm>
#include <cmath>

using QRgb = uint32_t;
using qsizetype = ptrdiff_t;

inline int qAlpha(QRgb rgba) { return (rgba >> 24) & 0xff; }
inline int qRed(QRgb rgba) { return (rgba >> 16) & 0xff; }
inline int qGreen(QRgb rgba) { return (rgba >> 8) & 0xff; }
inline int qBlue(QRgb rgba) { return rgba & 0xff; }
inline QRgb qRgba(int r, int g, int b, int a) {
    return ((a & 0xff) << 24) | ((r & 0xff) << 16) | ((g & 0xff) << 8) | (b & 0xff);
}
template <typename T> inline const T& qMin(const T& a, const T& b) { return std::min(a, b); }
inline int qRound(double x) { return static_cast<int>(std::round(x)); }

namespace snow_canvas_filter_render {

struct ImageView {
    uint8_t* data = nullptr;
    int width = 0;
    int height = 0;
    qsizetype stride = 0;
};

struct ConstImageView {
    const uint8_t* data = nullptr;
    int width = 0;
    int height = 0;
    qsizetype stride = 0;
};

struct AlphaView {
    const uint8_t* data = nullptr;
    int width = 0;
    int height = 0;
    qsizetype stride = 0;
};

}
