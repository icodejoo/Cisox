cmake_minimum_required(VERSION 3.10)
project(Target2_Palette CXX)
set(CMAKE_CXX_STANDARD 17)
add_executable(palette_baseline main.cpp fast_color_lite.cpp palette_generate.cpp)
