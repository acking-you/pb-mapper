# The GStreamer decoder module and a harness that drives it with recorded
# clips, outside Flutter. scripts/test/linux-desktop-video.sh builds and runs
# it; the app build (../CMakeLists.txt) does not include it.
cmake_minimum_required(VERSION 3.13)
project(desktop_video_test LANGUAGES CXX)

find_package(PkgConfig REQUIRED)
find_package(Threads REQUIRED)
pkg_check_modules(GSTREAMER REQUIRED IMPORTED_TARGET
  gstreamer-1.0 gstreamer-app-1.0 gstreamer-video-1.0)

set(RUNNER_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../runner")

# The module exactly as the runner builds it.
add_library(pb_mapper_desktop_video MODULE "${RUNNER_DIR}/desktop_video_gst.cc")
target_compile_features(pb_mapper_desktop_video PUBLIC cxx_std_14)
target_compile_options(pb_mapper_desktop_video PRIVATE -Wall -Werror)
set_target_properties(pb_mapper_desktop_video PROPERTIES
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON)
target_link_libraries(pb_mapper_desktop_video PRIVATE
  PkgConfig::GSTREAMER Threads::Threads)

add_executable(desktop_video_test "desktop_video_test.cc")
target_compile_features(desktop_video_test PUBLIC cxx_std_14)
target_compile_options(desktop_video_test PRIVATE -Wall -Werror)
target_include_directories(desktop_video_test PRIVATE "${RUNNER_DIR}")
target_compile_definitions(desktop_video_test PRIVATE
  PB_VIDEO_MODULE_PATH="$<TARGET_FILE:pb_mapper_desktop_video>")
target_link_libraries(desktop_video_test PRIVATE ${CMAKE_DL_LIBS} Threads::Threads)
add_dependencies(desktop_video_test pb_mapper_desktop_video)
