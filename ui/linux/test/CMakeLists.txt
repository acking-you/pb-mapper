# The GStreamer decoder module and a harness that drives it with recorded
# clips, outside Flutter, converting each picture with the runner's GL code
# in a GL ES context with no surface that stands in for Flutter's; and a unit
# test of how the module places decoders on GPUs, which needs neither.
# scripts/test/linux-desktop-video.sh builds and runs them; the app build
# (../CMakeLists.txt) does not include them.
cmake_minimum_required(VERSION 3.13)
project(desktop_video_test LANGUAGES CXX)

find_package(PkgConfig REQUIRED)
find_package(Threads REQUIRED)
pkg_check_modules(GSTREAMER REQUIRED IMPORTED_TARGET
  gstreamer-1.0 gstreamer-app-1.0 gstreamer-video-1.0 gstreamer-allocators-1.0)
# libepoxy resolves EGL and GL ES at run time, as the runner does.
pkg_check_modules(EPOXY REQUIRED IMPORTED_TARGET epoxy)

set(RUNNER_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../runner")

# The module exactly as the runner builds it.
add_library(pb_mapper_desktop_video MODULE "${RUNNER_DIR}/desktop_video_gst.cc"
  "${RUNNER_DIR}/desktop_video_device.cc")
target_compile_features(pb_mapper_desktop_video PUBLIC cxx_std_14)
target_compile_options(pb_mapper_desktop_video PRIVATE -Wall -Werror)
set_target_properties(pb_mapper_desktop_video PROPERTIES
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON)
target_link_libraries(pb_mapper_desktop_video PRIVATE
  PkgConfig::GSTREAMER Threads::Threads)

add_executable(desktop_video_test "desktop_video_test.cc" "${RUNNER_DIR}/desktop_video_gl.cc")
target_compile_features(desktop_video_test PUBLIC cxx_std_14)
target_compile_options(desktop_video_test PRIVATE -Wall -Werror)
target_include_directories(desktop_video_test PRIVATE "${RUNNER_DIR}")
target_compile_definitions(desktop_video_test PRIVATE
  PB_VIDEO_MODULE_PATH="$<TARGET_FILE:pb_mapper_desktop_video>")
target_link_libraries(desktop_video_test PRIVATE
  ${CMAKE_DL_LIBS} Threads::Threads PkgConfig::EPOXY)
add_dependencies(desktop_video_test pb_mapper_desktop_video)

add_executable(desktop_video_device_test "desktop_video_device_test.cc" "${RUNNER_DIR}/desktop_video_device.cc")
target_compile_features(desktop_video_device_test PUBLIC cxx_std_14)
target_compile_options(desktop_video_device_test PRIVATE -Wall -Werror)
target_include_directories(desktop_video_device_test PRIVATE "${RUNNER_DIR}")
