#include <iostream>
#include <string>
#include <regex>
#include <cctype>
#include <algorithm>
#include "nlohmann_json.hpp"

using json = nlohmann::json;

struct ConfigurationNormalization {
    json value;
    bool valid = false;
    bool changed = false;
};

// Helper: Trim string spaces (similar to QString::trimmed)
std::string trimmed(const std::string& str) {
    auto start = std::find_if_not(str.begin(), str.end(), [](unsigned char c) { return std::isspace(c); });
    auto end = std::find_if_not(str.rbegin(), str.rend(), [](unsigned char c) { return std::isspace(c); }).base();
    return (start < end) ? std::string(start, end) : std::string();
}

// Helper: To Lower (similar to QString::toLower)
std::string toLower(const std::string& str) {
    std::string result = str;
    std::transform(result.begin(), result.end(), result.begin(),
                   [](unsigned char c){ return std::tolower(c); });
    return result;
}

// Helper: To Upper (similar to QString::toUpper)
std::string toUpper(const std::string& str) {
    std::string result = str;
    std::transform(result.begin(), result.end(), result.begin(),
                   [](unsigned char c){ return std::toupper(c); });
    return result;
}

// 1. normalizeIntegerRange
ConfigurationNormalization normalizeIntegerRange(const json& value, int minimum, int maximum) {
    int integer = 0;
    // isInteger semantics: must be integer type
    if (!value.is_number_integer()) {
        return {};
    }
    integer = value.get<int>();
    if (integer < minimum || integer > maximum) {
        return {};
    }
    return {integer, true, false};
}

// 2. normalizeTheme
ConfigurationNormalization normalizeTheme(const json& value) {
    if (!value.is_string()) {
        return {};
    }
    const std::string original = value.get<std::string>();
    const std::string normalized = toLower(trimmed(original));
    if (normalized != "system" && normalized != "light" && normalized != "dark") {
        return {};
    }
    return {normalized, true, normalized != original};
}

// 3. normalizeRgbaColor
ConfigurationNormalization normalizeRgbaColor(const json& value) {
    if (!value.is_string()) {
        return {};
    }
    const std::string original = value.get<std::string>();
    const std::string normalized = toUpper(trimmed(original));
    static const std::regex pattern("^#[0-9A-F]{8}$");
    if (!std::regex_match(normalized, pattern)) {
        return {};
    }
    return {normalized, true, normalized != original};
}

// 4. normalizeFilenameFormat
ConfigurationNormalization normalizeFilenameFormat(const json& value) {
    if (!value.is_string()) {
        return {};
    }
    const std::string original = value.get<std::string>();
    const std::string normalized = trimmed(original);
    static const std::regex invalidCharacters("[\\\\/:*?\"<>|]");
    if (normalized.empty() || std::regex_search(normalized, invalidCharacters)) {
        return {};
    }
    return {normalized, true, normalized != original};
}

void printResult(const std::string& name, const json& input, const ConfigurationNormalization& result) {
    std::cout << "[" << name << "] Input: " << input.dump() 
              << " -> Valid: " << (result.valid ? "true" : "false")
              << ", Changed: " << (result.changed ? "true" : "false")
              << ", Output: " << (result.valid ? result.value.dump() : "null") << std::endl;
}

int main() {
    // 1. normalizeIntegerRange
    std::cout << "--- normalizeIntegerRange (min:10, max:100) ---\n";
    printResult("Valid", json(50), normalizeIntegerRange(json(50), 10, 100));
    printResult("InvalidType", json("50"), normalizeIntegerRange(json("50"), 10, 100));
    printResult("InvalidFloat", json(50.5), normalizeIntegerRange(json(50.5), 10, 100));
    printResult("OutOfBounds", json(150), normalizeIntegerRange(json(150), 10, 100));

    // 2. normalizeTheme
    std::cout << "\n--- normalizeTheme ---\n";
    printResult("Valid", json("dark"), normalizeTheme(json("dark")));
    printResult("ValidWithSpacesAndCaps", json("  LiGHT "), normalizeTheme(json("  LiGHT ")));
    printResult("InvalidType", json(123), normalizeTheme(json(123)));
    printResult("InvalidValue", json("blue"), normalizeTheme(json("blue")));

    // 3. normalizeRgbaColor
    std::cout << "\n--- normalizeRgbaColor ---\n";
    printResult("Valid", json("#AABBCCDD"), normalizeRgbaColor(json("#AABBCCDD")));
    printResult("ValidWithSpacesAndLower", json(" #aabbccdd "), normalizeRgbaColor(json(" #aabbccdd ")));
    printResult("InvalidType", json(123), normalizeRgbaColor(json(123)));
    printResult("InvalidLength", json("#AABBCC"), normalizeRgbaColor(json("#AABBCC")));
    printResult("InvalidChars", json("#AABBCCZG"), normalizeRgbaColor(json("#AABBCCZG")));

    // 4. normalizeFilenameFormat
    std::cout << "\n--- normalizeFilenameFormat ---\n";
    printResult("Valid", json("my_screenshot_%Y%m%d"), normalizeFilenameFormat(json("my_screenshot_%Y%m%d")));
    printResult("ValidWithSpaces", json("  file  "), normalizeFilenameFormat(json("  file  ")));
    printResult("InvalidType", json(123), normalizeFilenameFormat(json(123)));
    printResult("Empty", json("   "), normalizeFilenameFormat(json("   ")));
    printResult("InvalidChars", json("file:name"), normalizeFilenameFormat(json("file:name")));

    return 0;
}
