// 最小 Qt 兼容层：仅覆盖 snow_canvas_filter_render.cpp 等滤镜源码用到的 API，
// 让真实 C++ 内核不依赖 Qt 即可用 MSVC 编译，产出黄金样本。
#pragma once
#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <thread>
#include <vector>

using qsizetype = std::ptrdiff_t;
using qreal = double;
using uchar = unsigned char;
using QRgb = std::uint32_t;

#define Q_UNUSED(x) (void)x;

inline int qAlpha(QRgb rgba) { return (rgba >> 24) & 0xff; }
inline int qRed(QRgb rgba) { return (rgba >> 16) & 0xff; }
inline int qGreen(QRgb rgba) { return (rgba >> 8) & 0xff; }
inline int qBlue(QRgb rgba) { return rgba & 0xff; }
inline QRgb qRgba(int r, int g, int b, int a) {
    return ((a & 0xff) << 24) | ((r & 0xff) << 16) | ((g & 0xff) << 8) | (b & 0xff);
}
// 与 Qt 的 qRound(double) 完全一致
inline int qRound(double d) { return d >= 0.0 ? int(d + 0.5) : int(d - 0.5); }
inline int qCeil(double v) { return int(std::ceil(v)); }
template <typename T> constexpr const T& qMin(const T& a, const T& b) { return (a < b) ? a : b; }
template <typename T> constexpr const T& qMax(const T& a, const T& b) { return (a < b) ? b : a; }
template <typename T> constexpr const T& qBound(const T& min, const T& val, const T& max) {
    return qMax(min, qMin(max, val));
}

class QThread {
  public:
    static int idealThreadCount() { return int(std::thread::hardware_concurrency()); }
};

class QSize {
  public:
    QSize() = default;
    QSize(int w, int h) : m_w(w), m_h(h) {}
    int width() const { return m_w; }
    int height() const { return m_h; }
    bool isEmpty() const { return m_w <= 0 || m_h <= 0; }
    bool operator==(const QSize& o) const { return m_w == o.m_w && m_h == o.m_h; }
    bool operator!=(const QSize& o) const { return !(*this == o); }

  private:
    int m_w = -1, m_h = -1;
};

class QPoint {
  public:
    QPoint() = default;
    QPoint(int x, int y) : m_x(x), m_y(y) {}
    int x() const { return m_x; }
    int y() const { return m_y; }

  private:
    int m_x = 0, m_y = 0;
};

class QPointF {
  public:
    QPointF() = default;
    QPointF(double x, double y) : m_x(x), m_y(y) {}
    double x() const { return m_x; }
    double y() const { return m_y; }

  private:
    double m_x = 0, m_y = 0;
};

// 语义同 QRect：右/下为“含”边界（right = x + w - 1）
class QRect {
  public:
    QRect() = default;
    QRect(int x, int y, int w, int h) : m_x(x), m_y(y), m_w(w), m_h(h) {}
    QRect(const QPoint& p, const QSize& s)
        : m_x(p.x()), m_y(p.y()), m_w(s.width()), m_h(s.height()) {}
    int left() const { return m_x; }
    int top() const { return m_y; }
    int right() const { return m_x + m_w - 1; }
    int bottom() const { return m_y + m_h - 1; }
    int width() const { return m_w; }
    int height() const { return m_h; }
    bool isEmpty() const { return m_w <= 0 || m_h <= 0; }
    bool operator==(const QRect& o) const {
        return (isEmpty() && o.isEmpty()) ||
               (m_x == o.m_x && m_y == o.m_y && m_w == o.m_w && m_h == o.m_h);
    }
    bool operator!=(const QRect& o) const { return !(*this == o); }
    QRect adjusted(int dl, int dt, int dr, int db) const {
        return QRect(m_x + dl, m_y + dt, m_w + dr - dl, m_h + db - dt);
    }
    QRect intersected(const QRect& o) const {
        if (isEmpty() || o.isEmpty()) return QRect();
        const int l = std::max(left(), o.left()), t = std::max(top(), o.top());
        const int r = std::min(right(), o.right()), b = std::min(bottom(), o.bottom());
        if (l > r || t > b) return QRect();
        return QRect(l, t, r - l + 1, b - t + 1);
    }
    bool intersects(const QRect& o) const { return !intersected(o).isEmpty(); }
    QRect united(const QRect& o) const {
        if (isEmpty()) return o;
        if (o.isEmpty()) return *this;
        const int l = std::min(left(), o.left()), t = std::min(top(), o.top());
        const int r = std::max(right(), o.right()), b = std::max(bottom(), o.bottom());
        return QRect(l, t, r - l + 1, b - t + 1);
    }
    bool contains(const QRect& o) const {
        return !isEmpty() && !o.isEmpty() && o.left() >= left() && o.top() >= top() &&
               o.right() <= right() && o.bottom() <= bottom();
    }

  private:
    int m_x = 0, m_y = 0, m_w = 0, m_h = 0;
};

// 简化 QRegion：保存矩形列表；测试只用互不重叠的矩形，遍历顺序即输入顺序
class QRegion {
  public:
    QRegion() = default;
    QRegion(const QRect& r) {
        if (!r.isEmpty()) m_rects.push_back(r);
    }
    bool isEmpty() const { return m_rects.empty(); }
    QRect boundingRect() const {
        QRect result;
        for (const QRect& r : m_rects) result = result.united(r);
        return result;
    }
    QRegion intersected(const QRect& o) const {
        QRegion out;
        for (const QRect& r : m_rects) {
            const QRect i = r.intersected(o);
            if (!i.isEmpty()) out.m_rects.push_back(i);
        }
        return out;
    }
    QRegion intersected(const QRegion& o) const {
        QRegion out;
        for (const QRect& r : m_rects)
            for (const QRect& q : o.m_rects) {
                const QRect i = r.intersected(q);
                if (!i.isEmpty()) out.m_rects.push_back(i);
            }
        return out;
    }
    QRegion& operator+=(const QRegion& o) {
        m_rects.insert(m_rects.end(), o.m_rects.begin(), o.m_rects.end());
        return *this;
    }
    std::vector<QRect>::const_iterator begin() const { return m_rects.begin(); }
    std::vector<QRect>::const_iterator end() const { return m_rects.end(); }

  private:
    std::vector<QRect> m_rects;
};

// 带写时复制的 QImage 子集：拷贝共享存储，bits()/detach() 在多引用时深拷贝
class QImage {
  public:
    enum Format { Format_Invalid, Format_ARGB32_Premultiplied, Format_Alpha8 };
    QImage() = default;
    QImage(const QSize& size, Format format)
        : m_format(format), m_w(size.width()), m_h(size.height()) {
        m_bpl = bytesFor(m_w, format);
        m_owner = std::make_shared<std::vector<uchar>>(std::size_t(m_bpl) * std::size_t(m_h), 0);
        m_data = m_owner->data();
    }
    // 外部数据（不拥有）
    QImage(uchar* data, int w, int h, qsizetype bpl, Format format)
        : m_format(format), m_w(w), m_h(h), m_bpl(bpl), m_data(data) {}
    bool isNull() const { return m_data == nullptr; }
    Format format() const { return m_format; }
    int width() const { return m_w; }
    int height() const { return m_h; }
    QSize size() const { return QSize(m_w, m_h); }
    QRect rect() const { return QRect(0, 0, m_w, m_h); }
    qsizetype bytesPerLine() const { return m_bpl; }
    qsizetype sizeInBytes() const { return m_bpl * m_h; }
    qreal devicePixelRatio() const { return m_dpr; }
    void setDevicePixelRatio(qreal r) { m_dpr = r; }
    const uchar* constBits() const { return m_data; }
    uchar* bits() {
        detach();
        return m_data;
    }
    const uchar* constScanLine(int y) const { return m_data + qsizetype(y) * m_bpl; }
    uchar* scanLine(int y) {
        detach();
        return m_data + qsizetype(y) * m_bpl;
    }
    QImage convertToFormat(Format) const { return *this; }
    void detach() {
        if (m_owner && m_owner.use_count() > 1) {
            auto copy = std::make_shared<std::vector<uchar>>(*m_owner);
            m_owner = copy;
            m_data = m_owner->data();
        }
    }
    static qsizetype bytesFor(int w, Format f) {
        return f == Format_Alpha8 ? ((qsizetype(w) + 3) & ~qsizetype(3)) : qsizetype(w) * 4;
    }

  private:
    Format m_format = Format_Invalid;
    int m_w = 0, m_h = 0;
    qsizetype m_bpl = 0;
    qreal m_dpr = 1.0;
    std::shared_ptr<std::vector<uchar>> m_owner;
    uchar* m_data = nullptr;
};
