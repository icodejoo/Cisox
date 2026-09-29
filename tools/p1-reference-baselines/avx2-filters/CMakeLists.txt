cmake_minimum_required(VERSION 3.10)
project(Target1_AVX2 CXX)

set(CMAKE_CXX_STANDARD 17)

if(MSVC)
    add_compile_options(/arch:AVX2)
else()
    add_compile_options(-mavx2)
endif()

include_directories(.)

add_executable(avx2_baseline 
    main.cpp
    snow_canvas_filter_avx2.cpp
    snow_canvas_pen_mask_avx2.cpp
)
