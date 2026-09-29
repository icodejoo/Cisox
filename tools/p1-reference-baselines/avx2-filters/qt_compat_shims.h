#pragma once
#include <cmath>
inline int qRound(double x) { return static_cast<int>(std::round(x)); }
template <typename T> inline const T& qMin(const T& a, const T& b) { return std::min(a, b); }
