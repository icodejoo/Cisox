// 画布滤镜黄金样本生成器：直接编译真实的 snow_canvas_filter_render.cpp / snow_canvas_filter_avx2.cpp /
// snow_canvas_pen_mask_avx2.cpp（Qt 用 qt_shim 替代），按 cases.txt 生成输出并写入 golden.bin。
//
// 用法：
//   golden_main <cases.txt> <golden.bin>   生成黄金样本
//   golden_main bench [宽 高]              4K 单线程耗时（标量 / AVX2）
//
// 输入图像由固定种子的 xorshift32 生成（Rust 测试用同一算法复现，因此黄金文件只存输出）。
#include "snow_canvas_filter_render.h"
#include "snow_canvas_pen_mask_avx2.h"

#include <chrono>
#include <cstdio>
#include <fstream>
#include <functional>
#include <iostream>
#include <map>
#include <sstream>
#include <string>
#include <vector>

using namespace snow_canvas_filter_render;

namespace {

// xorshift32 伪随机数（种子为 0 时改用固定常量）
struct Rng {
    std::uint32_t s;
    explicit Rng(std::uint32_t seed) : s(seed ? seed : 0x9E3779B9u) {}
    std::uint32_t next() {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        return s;
    }
};

// 生成合法的预乘 ARGB32 图像（各颜色通道不超过 alpha）
QImage makeImage(int w, int h, std::uint32_t seed) {
    QImage image(QSize(w, h), QImage::Format_ARGB32_Premultiplied);
    Rng rng(seed);
    for (int y = 0; y < h; ++y) {
        auto* line = reinterpret_cast<QRgb*>(image.scanLine(y));
        for (int x = 0; x < w; ++x) {
            const std::uint32_t a0 = rng.next();
            const int raw = int(a0 & 0xff);
            const int kind = int((a0 >> 8) & 3);
            const int a = kind < 2 ? 255 : (kind == 2 ? raw : ((raw & 1) ? 255 : 0));
            const std::uint32_t c = rng.next();
            const int r = std::min(int(c & 0xff), a);
            const int g = std::min(int((c >> 8) & 0xff), a);
            const int b = std::min(int((c >> 16) & 0xff), a);
            line[x] = qRgba(r, g, b, a);
        }
    }
    return image;
}

// 生成 Alpha8 遮罩（0 / 255 / 随机各约 1/4、1/4、1/2）
QImage makeMask(int w, int h, std::uint32_t seed) {
    QImage image(QSize(w, h), QImage::Format_Alpha8);
    Rng rng(seed);
    for (int y = 0; y < h; ++y) {
        uchar* line = image.scanLine(y);
        for (int x = 0; x < w; ++x) {
            const std::uint32_t v = rng.next();
            const int kind = int((v >> 8) & 3);
            line[x] = uchar(kind == 0 ? 0 : (kind == 1 ? 255 : (v & 0xff)));
        }
    }
    return image;
}

// 把图像像素按行紧凑追加到输出（无行填充）
void appendImage(std::vector<uchar>& out, const QImage& image) {
    for (int y = 0; y < image.height(); ++y) {
        const uchar* line = image.constScanLine(y);
        out.insert(out.end(), line, line + std::size_t(image.width()) * 4);
    }
}

void appendInt32(std::vector<uchar>& out, std::int32_t v) {
    for (int i = 0; i < 4; ++i) out.push_back(uchar((std::uint32_t(v) >> (8 * i)) & 0xff));
}

// 读取 "type strength block sigma radius dpr ox oy fs" 到 Parameters/Options
void readParams(std::istringstream& in, Parameters& p, ExecutionOptions& o) {
    int fs = 0;
    in >> p.type >> p.strength >> p.logicalBlockSize >> p.logicalSigma >> p.logicalSamplingRadius >>
        p.devicePixelRatio;
    double ox = 0, oy = 0;
    in >> ox >> oy >> fs;
    p.gridOriginInImage = QPointF(ox, oy);
    o.forceScalar = fs != 0;
    o.singleThreaded = true;
}

void writeRecord(std::ofstream& file, const std::string& name, const std::vector<uchar>& data) {
    const std::uint32_t nameLen = std::uint32_t(name.size());
    const std::uint32_t dataLen = std::uint32_t(data.size());
    file.write(reinterpret_cast<const char*>(&nameLen), 4);
    file.write(name.data(), std::streamsize(name.size()));
    file.write(reinterpret_cast<const char*>(&dataLen), 4);
    file.write(reinterpret_cast<const char*>(data.data()), std::streamsize(data.size()));
}

// 写“与对应 _fs0 用例输出相同”的标记记录（数据长度 0xFFFFFFFF，无数据）
void writeSameAsFs0(std::ofstream& file, const std::string& name) {
    const std::uint32_t nameLen = std::uint32_t(name.size());
    const std::uint32_t marker = 0xFFFFFFFFu;
    file.write(reinterpret_cast<const char*>(&nameLen), 4);
    file.write(name.data(), std::streamsize(name.size()));
    file.write(reinterpret_cast<const char*>(&marker), 4);
}

// 执行一条用例，返回输出字节；未知类型返回空并报错
bool runCase(const std::string& line, std::string& name, std::vector<uchar>& out) {
    std::istringstream in(line);
    std::string kind;
    in >> name >> kind;
    out.clear();
    if (kind == "apply") {
        int w, h;
        std::uint32_t seed;
        in >> w >> h >> seed;
        Parameters p;
        ExecutionOptions o;
        readParams(in, p, o);
        QImage image = makeImage(w, h, seed);
        apply(image, p, nullptr, o);
        appendImage(out, image);
    } else if (kind == "masked") {
        int w, h, mx, my, mw, mh, rx, ry, rw, rh;
        std::uint32_t seed, maskSeed;
        in >> w >> h >> seed >> maskSeed >> mx >> my >> mw >> mh >> rx >> ry >> rw >> rh;
        Parameters p;
        ExecutionOptions o;
        readParams(in, p, o);
        const QImage source = makeImage(w, h, seed);
        QImage destination = makeImage(w, h, seed + 1000003u);
        const QImage mask = makeMask(mw, mh, maskSeed);
        const bool ok =
            applyMasked(source, destination, mask, QPoint(mx, my), QRect(rx, ry, rw, rh), p,
                        nullptr, o);
        out.push_back(ok ? 1 : 0);
        appendImage(out, destination);
    } else if (kind == "rect") {
        int w, h, rx, ry, rw, rh;
        std::uint32_t seed;
        double opacity;
        in >> w >> h >> seed >> rx >> ry >> rw >> rh >> opacity;
        Parameters p;
        ExecutionOptions o;
        readParams(in, p, o);
        const QImage source = makeImage(w, h, seed);
        QImage destination = makeImage(w, h, seed + 1000003u);
        const bool ok = applyRect(source, destination, QRect(rx, ry, rw, rh), opacity, p, nullptr, o);
        out.push_back(ok ? 1 : 0);
        appendImage(out, destination);
    } else if (kind == "region") {
        int w, h, n;
        std::uint32_t seed;
        in >> w >> h >> seed >> n;
        QRegion region;
        for (int i = 0; i < n; ++i) {
            int rx, ry, rw, rh;
            in >> rx >> ry >> rw >> rh;
            region += QRegion(QRect(rx, ry, rw, rh));
        }
        Parameters p;
        ExecutionOptions o;
        readParams(in, p, o);
        const QImage source = makeImage(w, h, seed);
        QImage destination = makeImage(w, h, seed + 1000003u);
        const bool ok = applyRegion(source, destination, region, p, nullptr, o);
        out.push_back(ok ? 1 : 0);
        appendImage(out, destination);
    } else if (kind == "blend") {
        int w, h;
        std::uint32_t seed;
        double opacity;
        in >> w >> h >> seed >> opacity;
        QImage filtered = makeImage(w, h, seed);
        const QImage source = makeImage(w, h, seed + 1000003u);
        ExecutionOptions o;
        o.singleThreaded = true;
        blendOverSource(filtered, source, opacity, o);
        appendImage(out, filtered);
    } else if (kind == "plan") {
        Parameters p;
        p.type = 1;
        in >> p.logicalSigma >> p.devicePixelRatio;
        const GaussianBlurPlan plan = gaussianBlurPlan(p);
        appendInt32(out, plan.reductionFactor);
        for (int i = 0; i < 3; ++i) appendInt32(out, plan.radii[i]);
        appendInt32(out, plan.physicalSupportRadius);
        appendInt32(out, samplingRadiusPixels(p));
    } else if (kind == "samp") {
        Parameters p;
        in >> p.type >> p.logicalSigma >> p.logicalSamplingRadius >> p.devicePixelRatio;
        appendInt32(out, samplingRadiusPixels(p));
    } else if (kind == "pen") {
        int size, stride, bx, ex, by, ey, tl, tt;
        std::uint32_t seed;
        double ax, ay, bpx, bpy, outer;
        in >> size >> seed >> stride >> bx >> ex >> by >> ey >> ax >> ay >> bpx >> bpy >> outer >>
            tl >> tt;
        std::vector<uchar> alpha(std::size_t(stride) * std::size_t(size));
        Rng rng(seed);
        for (auto& v : alpha) v = uchar((rng.next() >> 16) & 0xff);
        snow_canvas_pen_mask_avx2::rasterizeCapsuleSegment(alpha.data(), stride, tl, tt, bx, ex, by,
                                                           ey, ax, ay, bpx, bpy, outer);
        out = alpha;
    } else {
        std::cerr << "未知用例类型: " << kind << "\n";
        return false;
    }
    return true;
}

double bestMs(int reps, const std::function<void()>& prepare, const std::function<void()>& run) {
    double best = 1e30;
    for (int i = 0; i < reps; ++i) {
        prepare();
        const auto begin = std::chrono::steady_clock::now();
        run();
        const auto end = std::chrono::steady_clock::now();
        best = std::min(best, std::chrono::duration<double, std::milli>(end - begin).count());
    }
    return best;
}

// 4K 耗时（单线程）；每个滤镜分别测标量与 AVX2
int runBench(int w, int h) {
    struct Item {
        const char* name;
        Parameters p;
    };
    auto make = [](std::uint32_t type, double strength, double block, double sigma, double radius) {
        Parameters p;
        p.type = type;
        p.strength = strength;
        p.logicalBlockSize = block;
        p.logicalSigma = sigma;
        p.logicalSamplingRadius = radius;
        return p;
    };
    const std::vector<Item> items = {
        {"mosaic_b16", make(0, 1, 16, 0, 0)},   {"blur_s2", make(1, 1, 0, 2, 0)},
        {"blur_s8", make(1, 1, 0, 8, 0)},       {"blur_s32", make(1, 1, 0, 32, 0)},
        {"grayscale", make(2, 1, 0, 0, 0)},     {"invert", make(3, 1, 0, 0, 0)},
        {"emboss_r1", make(4, 1, 0, 0, 1)},
    };
    const QImage original = makeImage(w, h, 7);
    for (const Item& item : items) {
        for (int scalar = 1; scalar >= 0; --scalar) {
            ExecutionOptions o;
            o.forceScalar = scalar != 0;
            o.singleThreaded = true;
            QImage image;
            const double ms = bestMs(
                3,
                [&] {
                    image = QImage(QSize(w, h), QImage::Format_ARGB32_Premultiplied);
                    std::memcpy(image.bits(), original.constBits(), std::size_t(image.sizeInBytes()));
                },
                [&] { apply(image, item.p, nullptr, o); });
            std::printf("bench,%s,%s,%.3f\n", item.name, scalar ? "scalar" : "avx2", ms);
        }
    }
    return 0;
}

} // namespace

int main(int argc, char** argv) {
    if (argc >= 2 && std::string(argv[1]) == "bench") {
        const int w = argc >= 4 ? std::atoi(argv[2]) : 3840;
        const int h = argc >= 4 ? std::atoi(argv[3]) : 2160;
        return runBench(w, h);
    }
    if (argc < 3) {
        std::cerr << "用法: golden_main <cases.txt> <golden.bin> | golden_main bench [宽 高]\n";
        return 2;
    }
    std::ifstream cases(argv[1]);
    std::ofstream golden(argv[2], std::ios::binary);
    if (!cases || !golden) {
        std::cerr << "无法打开输入/输出文件\n";
        return 2;
    }
    std::string line;
    int count = 0;
    int differing = 0;
    std::map<std::string, std::vector<uchar>> scalarOutputs;
    while (std::getline(cases, line)) {
        if (line.empty() || line[0] == '#') continue;
        std::string name;
        std::vector<uchar> out;
        if (!runCase(line, name, out)) return 1;
        // 以 _fs0 结尾的用例记下输出；对应 _fs1 若与之逐字节相同则只写“同 fs0”标记，减小文件体积
        const std::string fs0Suffix = "_fs0";
        const std::string fs1Suffix = "_fs1";
        if (name.size() > 4 && name.compare(name.size() - 4, 4, fs0Suffix) == 0) {
            scalarOutputs[name] = out;
        } else if (name.size() > 4 && name.compare(name.size() - 4, 4, fs1Suffix) == 0) {
            const auto twin = scalarOutputs.find(name.substr(0, name.size() - 4) + fs0Suffix);
            if (twin != scalarOutputs.end() && twin->second == out) {
                writeSameAsFs0(golden, name);
                ++count;
                continue;
            }
            if (twin != scalarOutputs.end()) {
                std::cout << "C++ 标量与 AVX2 输出不同: " << name << "\n";
                ++differing;
            }
        }
        writeRecord(golden, name, out);
        ++count;
    }
    std::cout << "已生成 " << count << " 条黄金样本，C++ 标量与 AVX2 不一致 " << differing << " 条\n";
    return 0;
}
