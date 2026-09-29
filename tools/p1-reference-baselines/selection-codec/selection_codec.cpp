#include <iostream>
#include <string>
#include <vector>
#include <sstream>

// Dummy types
struct Rect {
    int x, y, width, height;
    bool operator==(const Rect& r) const {
        return x == r.x && y == r.y && width == r.width && height == r.height;
    }
    bool operator!=(const Rect& r) const { return !(*this == r); }
};

struct Region {
    std::vector<Rect> rects;
    bool isEmpty() const { return rects.empty(); }
    Rect boundingRect() const {
        if (rects.empty()) return {0,0,0,0};
        int minX = rects[0].x;
        int minY = rects[0].y;
        int maxX = rects[0].x + rects[0].width;
        int maxY = rects[0].y + rects[0].height;
        for (const auto& r : rects) {
            if (r.x < minX) minX = r.x;
            if (r.y < minY) minY = r.y;
            if (r.x + r.width > maxX) maxX = r.x + r.width;
            if (r.y + r.height > maxY) maxY = r.y + r.height;
        }
        return {minX, minY, maxX - minX, maxY - minY};
    }
};

struct PersistedSelection {
    Rect rectangle;
    Region region;
    int cornerRadius = 0;
    int shadowWidth = 0;
    std::string shadowColor;
    bool lockAspectRatio = false;
    bool lockDragAspectRatio = false;
};

struct PersistedWindowGeometry {
    Rect normalGeometry;
    bool maximized = false;
};

// Simple JSON building
std::string toJsonString(const Rect& rect) {
    std::ostringstream ss;
    ss << "{\"x\":" << rect.x << ",\"y\":" << rect.y 
       << ",\"width\":" << rect.width << ",\"height\":" << rect.height << "}";
    return ss.str();
}

std::string persistedSelectionToJson(const PersistedSelection& selection) {
    std::ostringstream ss;
    ss << "{";
    ss << "\"rectangle\":" << toJsonString(selection.rectangle) << ",";
    ss << "\"corner_radius\":" << selection.cornerRadius << ",";
    ss << "\"shadow_width\":" << selection.shadowWidth << ",";
    ss << "\"shadow_color\":\"" << selection.shadowColor << "\",";
    ss << "\"lock_aspect_ratio\":" << (selection.lockAspectRatio ? "true" : "false") << ",";
    ss << "\"lock_drag_aspect_ratio\":" << (selection.lockDragAspectRatio ? "true" : "false");
    
    if (!selection.region.isEmpty() && selection.region.boundingRect() != selection.rectangle) {
        ss << ",\"regions\":[";
        for (size_t i = 0; i < selection.region.rects.size(); ++i) {
            ss << toJsonString(selection.region.rects[i]);
            if (i < selection.region.rects.size() - 1) ss << ",";
        }
        ss << "]";
    }
    ss << "}";
    return ss.str();
}

std::string windowGeometryToJson(const Rect& normalGeometry, bool maximized) {
    std::ostringstream ss;
    ss << "{";
    ss << "\"x\":" << normalGeometry.x << ",";
    ss << "\"y\":" << normalGeometry.y << ",";
    ss << "\"width\":" << normalGeometry.width << ",";
    ss << "\"height\":" << normalGeometry.height << ",";
    ss << "\"maximized\":" << (maximized ? "true" : "false");
    ss << "}";
    return ss.str();
}

int main() {
    PersistedSelection sel1;
    sel1.rectangle = {10, 20, 100, 200};
    sel1.cornerRadius = 4;
    sel1.shadowWidth = 10;
    sel1.shadowColor = "#ff000000";
    sel1.lockAspectRatio = true;
    sel1.lockDragAspectRatio = false;
    
    PersistedSelection sel2 = sel1;
    sel2.region.rects.push_back({10, 20, 50, 200});
    sel2.region.rects.push_back({60, 20, 50, 200});
    
    std::cout << "PersistedSelection 1:\n" << persistedSelectionToJson(sel1) << "\n\n";
    std::cout << "PersistedSelection 2 (matches rect):\n" << persistedSelectionToJson(sel2) << "\n\n";
    
    PersistedSelection sel3 = sel2;
    sel3.rectangle = {0, 0, 100, 200}; // Mismatch on purpose to trigger regions array
    std::cout << "PersistedSelection 3 (mismatch rect):\n" << persistedSelectionToJson(sel3) << "\n\n";
    
    std::cout << "WindowGeometry 1:\n" << windowGeometryToJson({0, 0, 1920, 1080}, false) << "\n\n";
    std::cout << "WindowGeometry 2:\n" << windowGeometryToJson({-10, 50, 800, 600}, true) << "\n\n";
    
    return 0;
}
