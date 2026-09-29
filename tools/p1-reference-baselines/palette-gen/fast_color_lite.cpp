#include <cstdio>
#include <cmath>
#include <string>
inline double roundToTwo(double x) { return std::round(x * 100.0) / 100.0; }
#include "fast_color_lite.h"



#include <cmath>
#include <regex>
#include <vector>

namespace adqt::theme {

namespace {

double roundToTwo(double value) { return std::round(value * 100.0) / 100.0; }

}  // namespace

FastColorLite::FastColorLite() : r_(0), g_(0), b_(0), a_(1.0), valid_(true) {}

FastColorLite::FastColorLite(const std::string& input) : FastColorLite() {
  const std::string trimmed = input;
  if (trimmed.empty()) {
    valid_ = false;
    return;
  }

  if (parseHex(trimmed) || parseRgb(trimmed)) {
    valid_ = true;
    return;
  }

  valid_ = false;
}

FastColorLite::FastColorLite(int r, int g, int b, double a)
    : r_(clampChannel(r)),
      g_(clampChannel(g)),
      b_(clampChannel(b)),
      a_(clampUnit(a)),
      valid_(true) {}

FastColorLite FastColorLite::fromHsv(const HsvColor& hsv) {
  double h = std::fmod(hsv.h, 360.0);
  if (h < 0.0) {
    h += 360.0;
  }

  const double s = clampUnit(hsv.s);
  const double v = clampUnit(hsv.v);

  int r = static_cast<int>(std::round(v * 255.0));
  int g = r;
  int b = r;

  if (s > 0.0) {
    const double hh = h / 60.0;
    const int i = static_cast<int>(std::floor(hh));
    const double ff = hh - i;
    const int p = static_cast<int>(std::round(v * (1.0 - s) * 255.0));
    const int q = static_cast<int>(std::round(v * (1.0 - (s * ff)) * 255.0));
    const int t = static_cast<int>(std::round(v * (1.0 - (s * (1.0 - ff))) * 255.0));

    switch (i) {
      case 0:
        g = t;
        b = p;
        break;
      case 1:
        r = q;
        b = p;
        break;
      case 2:
        r = p;
        b = t;
        break;
      case 3:
        r = p;
        g = q;
        break;
      case 4:
        r = t;
        g = p;
        break;
      case 5:
      default:
        g = p;
        b = q;
        break;
    }
  }

  return FastColorLite(r, g, b, hsv.a);
}

bool FastColorLite::isValid() const { return valid_; }

int FastColorLite::red() const { return r_; }
int FastColorLite::green() const { return g_; }
int FastColorLite::blue() const { return b_; }
double FastColorLite::alpha() const { return a_; }

FastColorLite FastColorLite::setAlpha(double alpha) const {
  return FastColorLite(r_, g_, b_, clampUnit(alpha));
}

HsvColor FastColorLite::toHsv() const {
  const int maxChannel = std::max({r_, g_, b_});
  const int minChannel = std::min({r_, g_, b_});
  const int delta = maxChannel - minChannel;

  double h = 0.0;
  if (delta != 0) {
    if (r_ == maxChannel) {
      h = 60.0 * (((g_ - b_) / static_cast<double>(delta)) + (g_ < b_ ? 6.0 : 0.0));
    } else if (g_ == maxChannel) {
      h = 60.0 * (((b_ - r_) / static_cast<double>(delta)) + 2.0);
    } else {
      h = 60.0 * (((r_ - g_) / static_cast<double>(delta)) + 4.0);
    }
  }

  h = std::round(h);

  double s = 0.0;
  if (maxChannel != 0) {
    s = delta / static_cast<double>(maxChannel);
  }

  const double v = maxChannel / 255.0;

  return HsvColor{h, s, v, a_};
}

FastColorLite FastColorLite::darken(double amountPercent) const {
  const HsvColor hsv = toHsv();
  const int maxChannel = std::max({r_, g_, b_});
  const int minChannel = std::min({r_, g_, b_});

  double lightness = (maxChannel + minChannel) / 510.0;
  lightness -= amountPercent / 100.0;
  lightness = clampUnit(lightness);

  return fromHsl(hsv.h, hsv.s, lightness, a_);
}

FastColorLite FastColorLite::lighten(double amountPercent) const {
  const HsvColor hsv = toHsv();
  const int maxChannel = std::max({r_, g_, b_});
  const int minChannel = std::min({r_, g_, b_});

  double lightness = (maxChannel + minChannel) / 510.0;
  lightness += amountPercent / 100.0;
  lightness = clampUnit(lightness);

  return fromHsl(hsv.h, hsv.s, lightness, a_);
}

FastColorLite FastColorLite::mix(const FastColorLite& other, double amountPercent) const {
  const double p = clamp(amountPercent / 100.0, 0.0, 1.0);
  const int r = static_cast<int>(std::round((other.r_ - r_) * p + r_));
  const int g = static_cast<int>(std::round((other.g_ - g_) * p + g_));
  const int b = static_cast<int>(std::round((other.b_ - b_) * p + b_));
  const double a = roundToTwo((other.a_ - a_) * p + a_);
  return FastColorLite(r, g, b, a);
}

std::string FastColorLite::toHexString() const {
  char buf[32];
  if (a_ >= 0.0 && a_ < 1.0) {
    int alpha = static_cast<int>(std::round(a_ * 255.0));
    snprintf(buf, sizeof(buf), "#%02x%02x%02x%02x", r_, g_, b_, alpha);
  } else {
    snprintf(buf, sizeof(buf), "#%02x%02x%02x", r_, g_, b_);
  }
  return std::string(buf);
}

std::string FastColorLite::toRgbString() const {
  char buf[64];
  if (a_ >= 1.0) {
    snprintf(buf, sizeof(buf), "rgb(%d,%d,%d)", r_, g_, b_);
  } else {
    snprintf(buf, sizeof(buf), "rgba(%d,%d,%d,%s)", r_, g_, b_, formatAlpha(a_).c_str());
  }
  return std::string(buf);
}

bool FastColorLite::parseHex(const std::string& input) {
  std::string hex = input;
  if (!hex.empty() && hex[0] == '#') {
    hex = hex.substr(1);
  }
  
  auto isHex = [](char c) {
      return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
  };
  auto hexVal = [](char c) -> int {
      if (c >= '0' && c <= '9') return c - '0';
      if (c >= 'a' && c <= 'f') return c - 'a' + 10;
      if (c >= 'A' && c <= 'F') return c - 'A' + 10;
      return 0;
  };

  if (hex.size() == 3 || hex.size() == 4) {
    std::string full_hex;
    for (char c : hex) { full_hex += c; full_hex += c; }
    hex = full_hex;
  }
  
  if (hex.size() == 6 || hex.size() == 8) {
    for (char c : hex) {
        if (!isHex(c)) return false;
    }
    r_ = (hexVal(hex[0]) << 4) | hexVal(hex[1]);
    g_ = (hexVal(hex[2]) << 4) | hexVal(hex[3]);
    b_ = (hexVal(hex[4]) << 4) | hexVal(hex[5]);
    if (hex.size() == 8) {
        a_ = ((hexVal(hex[6]) << 4) | hexVal(hex[7])) / 255.0;
    } else {
        a_ = 1.0;
    }
    return true;
  }
  return false;
}

bool FastColorLite::parseRgb(const std::string& input) {
  const std::regex prefixRe("^\\s*rgba?\\((.*)\\)\\s*$", std::regex::icase);
  std::smatch prefixMatch;
  if (!std::regex_match(input, prefixMatch, prefixRe)) {
    return false;
  }

  const std::string inside = prefixMatch[1].str();
  const std::regex numberRe("\\d*\\.?\\d+%?");
  auto words_begin = std::sregex_iterator(inside.begin(), inside.end(), numberRe);
  auto words_end = std::sregex_iterator();

  std::vector<std::string> values;
  for (std::sregex_iterator i = words_begin; i != words_end; ++i) {
    values.push_back(i->str());
  }

  if (values.size() < 3) {
    return false;
  }

  auto toDoubleSafe = [](const std::string& s, bool& ok) {
      char* end;
      double val = std::strtod(s.c_str(), &end);
      if (end == s.c_str()) {
          ok = false;
          return 0.0;
      }
      ok = true;
      return val;
  };

  auto toChannel = [&toDoubleSafe](const std::string& value, bool& ok) {
    if (!value.empty() && value.back() == '%') {
      const double pct = toDoubleSafe(value.substr(0, value.size() - 1), ok);
      return static_cast<int>(std::round(pct / 100.0 * 255.0));
    }
    return static_cast<int>(std::round(toDoubleSafe(value, ok)));
  };

  auto toAlpha = [&toDoubleSafe](const std::string& value, bool& ok) {
    if (!value.empty() && value.back() == '%') {
      return toDoubleSafe(value.substr(0, value.size() - 1), ok) / 100.0;
    }
    return toDoubleSafe(value, ok);
  };

  bool ok = true;
  int r = clampChannel(toChannel(values.at(0), ok));
  if (!ok) return false;
  int g = clampChannel(toChannel(values.at(1), ok));
  if (!ok) return false;
  int b = clampChannel(toChannel(values.at(2), ok));
  if (!ok) return false;

  double a = 1.0;
  if (values.size() >= 4) {
    a = clampUnit(toAlpha(values.at(3), ok));
    if (!ok) return false;
  }

  r_ = r;
  g_ = g;
  b_ = b;
  a_ = a;

  return true;
}
int FastColorLite::clampChannel(int value) { return static_cast<int>(clamp(value, 0.0, 255.0)); }
double FastColorLite::clampUnit(double value) { return clamp(value, 0.0, 1.0); }
double FastColorLite::clamp(double value, double minValue, double maxValue) {
  if (value < minValue) return minValue;
  if (value > maxValue) return maxValue;
  return value;
}
std::string FastColorLite::formatAlpha(double alpha) {
  char buf[32];
  snprintf(buf, sizeof(buf), "%.2f", alpha);
  std::string result(buf);
  while (!result.empty() && result.back() == '0') result.pop_back();
  if (!result.empty() && result.back() == '.') result.pop_back();
  return result;
}

FastColorLite FastColorLite::fromHsl(double h, double s, double l, double a) {
  h = std::fmod(h, 360.0);
  if (h < 0.0) {
    h += 360.0;
  }

  s = clampUnit(s);
  l = clampUnit(l);

  if (s <= 0.0) {
    const int rgb = static_cast<int>(std::round(l * 255.0));
    return FastColorLite(rgb, rgb, rgb, a);
  }

  double r = 0.0;
  double g = 0.0;
  double b = 0.0;

  const double huePrime = h / 60.0;
  const double chroma = (1.0 - std::abs((2.0 * l) - 1.0)) * s;
  const double second = chroma * (1.0 - std::abs(std::fmod(huePrime, 2.0) - 1.0));

  if (huePrime >= 0.0 && huePrime < 1.0) {
    r = chroma;
    g = second;
  } else if (huePrime >= 1.0 && huePrime < 2.0) {
    r = second;
    g = chroma;
  } else if (huePrime >= 2.0 && huePrime < 3.0) {
    g = chroma;
    b = second;
  } else if (huePrime >= 3.0 && huePrime < 4.0) {
    g = second;
    b = chroma;
  } else if (huePrime >= 4.0 && huePrime < 5.0) {
    r = second;
    b = chroma;
  } else {
    r = chroma;
    b = second;
  }

  const double mod = l - chroma / 2.0;

  return FastColorLite(static_cast<int>(std::round((r + mod) * 255.0)),
                       static_cast<int>(std::round((g + mod) * 255.0)),
                       static_cast<int>(std::round((b + mod) * 255.0)), a);
}

}  // namespace adqt::theme
