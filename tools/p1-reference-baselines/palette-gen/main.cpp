#include <iostream>
#include <string>
#include <vector>
#include "palette_generate.h"
#include "fast_color_lite.h"

int main() {
    std::vector<std::string> base_colors = {"#1677ff", "#f5222d", "#52c41a"};
    
    for (const auto& color : base_colors) {
        std::cout << "Base color: " << color << std::endl;
        std::cout << "Light theme palette:" << std::endl;
        auto light_palette = adqt::theme::generatePalette(color, false, "");
        for (size_t i = 0; i < light_palette.size(); ++i) {
            std::cout << "  " << i + 1 << ": " << light_palette[i] << std::endl;
        }
        
        std::cout << "Dark theme palette:" << std::endl;
        auto dark_palette = adqt::theme::generatePalette(color, true, "#141414");
        for (size_t i = 0; i < dark_palette.size(); ++i) {
            std::cout << "  " << i + 1 << ": " << dark_palette[i] << std::endl;
        }
        std::cout << std::endl;
    }
    
    // Add invalid input tests
    std::vector<std::string> invalid_colors = {"#zzzzzz", "#12", "", "abcdefg", "blue"};
    std::cout << "Testing invalid inputs:" << std::endl;
    for (const auto& color : invalid_colors) {
        adqt::theme::FastColorLite fc(color);
        if (fc.isValid()) {
            std::cout << "  " << (color.empty() ? "<empty>" : color) << " -> valid (unexpected)" << std::endl;
        } else {
            std::cout << "  " << (color.empty() ? "<empty>" : color) << " -> invalid (as expected)" << std::endl;
        }
    }

    // Test rgb/rgba
    std::cout << "\nTesting RGB/RGBA inputs:" << std::endl;
    std::vector<std::string> rgb_tests = {
        "rgb(255, 0, 0)",
        "rgba(0, 128, 255, 0.5)",
        "rgb(100%, 0%, 50%)",
        "rgb(255,0",
        "rgb(255)",
        "hsl(0,100%,50%)"
    };
    for (const auto& color : rgb_tests) {
        adqt::theme::FastColorLite fc(color);
        std::cout << "  " << color << " -> " << (fc.isValid() ? "valid, Hex: " + fc.toHexString() : "invalid") << std::endl;
    }

    return 0;
}
