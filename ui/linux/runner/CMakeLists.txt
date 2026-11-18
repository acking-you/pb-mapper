cmake_minimum_required(VERSION 3.13)
project(runner LANGUAGES CXX)

# Define the application target. To change its name, change BINARY_NAME in the
# top-level CMakeLists.txt, not the value here, or `flutter run` will no longer
# work.
#
# Any new source files that you add to the application should be added here.
add_executable(${BINARY_NAME}
  "main.cc"
  "my_application.cc"
  "desktop_video.cc"
  "${FLUTTER_MANAGED_DIR}/generated_plugin_registrant.cc"
)

# Apply the standard set of build settings. This can be removed for applications
# that need different build settings.
apply_standard_settings(${BINARY_NAME})

# Add preprocessor definitions for the application ID.
add_definitions(-DAPPLICATION_ID="${APPLICATION_ID}")

# Add dependency libraries. Add any application-specific dependencies here.
target_link_libraries(${BINARY_NAME} PRIVATE flutter)
target_link_libraries(${BINARY_NAME} PRIVATE PkgConfig::GTK)
# desktop_video.cc loads the decoder module and the FFI library at run time.
target_link_libraries(${BINARY_NAME} PRIVATE ${CMAKE_DL_LIBS})

target_include_directories(${BINARY_NAME} PRIVATE "${CMAKE_SOURCE_DIR}")

# The native video decoder module (desktop_video_gst.cc), built where the
# GStreamer development files are installed. The runner loads it at run time,
# so the app needs neither the module nor GStreamer to start; without them
# every remote desktop session is JPEG.
pkg_check_modules(GSTREAMER QUIET IMPORTED_TARGET
  gstreamer-1.0 gstreamer-app-1.0 gstreamer-video-1.0)
if(GSTREAMER_FOUND)
  find_package(Threads REQUIRED)
  add_library(pb_mapper_desktop_video MODULE "desktop_video_gst.cc")
  apply_standard_settings(pb_mapper_desktop_video)
  set_target_properties(pb_mapper_desktop_video PROPERTIES
    CXX_VISIBILITY_PRESET hidden
    VISIBILITY_INLINES_HIDDEN ON)
  target_link_libraries(pb_mapper_desktop_video PRIVATE
    PkgConfig::GSTREAMER Threads::Threads)
  message(STATUS "Native desktop video: GStreamer ${GSTREAMER_gstreamer-1.0_VERSION}")
else()
  message(STATUS "Native desktop video: GStreamer development files not found; the app decodes JPEG only")
endif()
