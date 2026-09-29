#include <iostream>
#include <vector>
#include "snow_canvas_filter_avx2.h"
#include "snow_canvas_pen_mask_avx2.h"

#define STB_IMAGE_WRITE_IMPLEMENTATION
#include "../stb_image_write.h"

int main() {
    int w = 256;
    int h = 256;
    std::vector<uint32_t> pixels(w * h);
    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            pixels[y * w + x] = qRgba(x, y, (x + y) / 2, 255);
        }
    }

    snow_canvas_filter_render::ImageView view;
    view.data = reinterpret_cast<uint8_t*>(pixels.data());
    view.width = w;
    view.height = h;
    view.stride = w * 4;

    snow_canvas_filter_render::detail::invertAvx2(view, 0, h, 255);

    stbi_write_png("test_invert.png", w, h, 4, pixels.data(), w * 4);
    
    // Test grayscale
    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            pixels[y * w + x] = qRgba(x, y, (x + y) / 2, 255);
        }
    }
    snow_canvas_filter_render::detail::grayscaleAvx2(view, 0, h, 255);
    stbi_write_png("test_grayscale.png", w, h, 4, pixels.data(), w * 4);

    std::cout << "Target 1 (AVX2) generated test_invert.png and test_grayscale.png." << std::endl;
    return 0;
}
