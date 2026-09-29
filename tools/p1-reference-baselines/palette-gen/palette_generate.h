#pragma once

#include <string>
#include <vector>

namespace adqt::theme {

std::vector<std::string> generatePalette(const std::string& color, bool darkTheme = false,
                                 const std::string& backgroundColor = "");

}  // namespace adqt::theme
