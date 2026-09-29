#pragma once

#include "snow_canvas_filter_render.h"

namespace snow_canvas_filter_render::detail {

/**
 * @brief 对整个图像应用 AVX2 优化的灰度滤镜。
 * @param image 待处理的图像视图 (ImageView)。
 * @param beginRow 开始处理的行号。
 * @param endRow 结束处理的行号。
 * @param mix 混合强度 (0-255)。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = grayscaleAvx2(imgView, 0, imgView.height(), 255);
 */
bool grayscaleAvx2(ImageView image, int beginRow, int endRow, int mix);

/**
 * @brief 对整个图像应用 AVX2 优化的反相滤镜。
 * @param image 待处理的图像视图 (ImageView)。
 * @param beginRow 开始处理的行号。
 * @param endRow 结束处理的行号。
 * @param mix 混合强度 (0-255)。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = invertAvx2(imgView, 0, imgView.height(), 128);
 */
bool invertAvx2(ImageView image, int beginRow, int endRow, int mix);

/**
 * @brief 对图像的指定矩形区域应用 AVX2 优化的灰度滤镜。
 * @param source 源图像视图。
 * @param destination 目标图像视图。
 * @param left 矩形左边界。
 * @param top 矩形上边界。
 * @param right 矩形右边界。
 * @param bottom 矩形下边界。
 * @param mix 混合强度 (0-255)。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = grayscaleRectAvx2(src, dst, 10, 10, 100, 100, 255);
 */
bool grayscaleRectAvx2(ConstImageView source, ImageView destination, int left, int top, int right,
                       int bottom, int mix);

/**
 * @brief 对图像的指定矩形区域应用 AVX2 优化的反相滤镜。
 * @param source 源图像视图。
 * @param destination 目标图像视图。
 * @param left 矩形左边界。
 * @param top 矩形上边界。
 * @param right 矩形右边界。
 * @param bottom 矩形下边界。
 * @param mix 混合强度 (0-255)。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = invertRectAvx2(src, dst, 0, 0, 50, 50, 255);
 */
bool invertRectAvx2(ConstImageView source, ImageView destination, int left, int top, int right,
                    int bottom, int mix);

/**
 * @brief 基于遮罩对图像的指定区域应用 AVX2 优化的灰度滤镜。
 * @param source 源图像视图。
 * @param destination 目标图像视图。
 * @param mask 遮罩视图 (AlphaView)。
 * @param maskOriginX 遮罩在目标图像中的 X 坐标。
 * @param maskOriginY 遮罩在目标图像中的 Y 坐标。
 * @param left 矩形左边界。
 * @param top 矩形上边界。
 * @param right 矩形右边界。
 * @param bottom 矩形下边界。
 * @param strengthMix 混合强度。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = grayscaleMaskedAvx2(src, dst, mask, 0, 0, 0, 0, 100, 100, 255);
 */
bool grayscaleMaskedAvx2(ConstImageView source, ImageView destination, AlphaView mask,
                         int maskOriginX, int maskOriginY, int left, int top, int right, int bottom,
                         int strengthMix);

/**
 * @brief 基于遮罩对图像的指定区域应用 AVX2 优化的反相滤镜。
 * @param source 源图像视图。
 * @param destination 目标图像视图。
 * @param mask 遮罩视图 (AlphaView)。
 * @param maskOriginX 遮罩在目标图像中的 X 坐标。
 * @param maskOriginY 遮罩在目标图像中的 Y 坐标。
 * @param left 矩形左边界。
 * @param top 矩形上边界。
 * @param right 矩形右边界。
 * @param bottom 矩形下边界。
 * @param strengthMix 混合强度。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = invertMaskedAvx2(src, dst, mask, 0, 0, 0, 0, 100, 100, 255);
 */
bool invertMaskedAvx2(ConstImageView source, ImageView destination, AlphaView mask, int maskOriginX,
                      int maskOriginY, int left, int top, int right, int bottom, int strengthMix);

/**
 * @brief 使用 AVX2 优化复制图像行。
 * @param source 源图像视图。
 * @param destination 目标图像视图。
 * @param beginRow 开始行号。
 * @param endRow 结束行号。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = copyRowsAvx2(src, dst, 0, 100);
 */
bool copyRowsAvx2(ConstImageView source, ImageView destination, int beginRow, int endRow);

/**
 * @brief 使用 AVX2 优化的 4 抽头下采样。
 * @param source 源图像视图。
 * @param sourceLeft 源矩形左边界。
 * @param sourceTop 源矩形上边界。
 * @param sourceRight 源矩形右边界。
 * @param sourceBottom 源矩形下边界。
 * @param destination 目标图像视图。
 * @param factor 降采样因子。
 * @param beginRow 开始处理的行号。
 * @param endRow 结束处理的行号。
 * @return 成功返回 true，否则返回 false。
 * @example
 *   bool success = downsampleFourTapAvx2(src, 0, 0, 100, 100, dst, 2, 0, 100);
 */
bool downsampleFourTapAvx2(ConstImageView source, int sourceLeft, int sourceTop, int sourceRight,
                           int sourceBottom, ImageView destination, int factor, int beginRow,
                           int endRow);
int interpolateAndBlendConstantAvx2(const QRgb* first, const QRgb* second, QRgb* destination,
                                    int count, int weight, int mix);
int interpolateAndBlendMaskedAvx2(const QRgb* first, const QRgb* second, QRgb* destination,
                                  const std::uint8_t* mask, int count, int weight);

} // namespace snow_canvas_filter_render::detail
