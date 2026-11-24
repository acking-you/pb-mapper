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
  "desktop_video_gl.cc"
  "desktop_pointer.cc"
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
# desktop_video_gl.cc converts decoded pictures in Flutter's EGL and GL ES
# context through libepoxy, which GTK and Flutter's embedder already load.
pkg_check_modules(EPOXY REQUIRED IMPORTED_TARGET epoxy)
target_link_libraries(${BINARY_NAME} PRIVATE PkgConfig::EPOXY)

target_include_directories(${BINARY_NAME} PRIVATE "${CMAKE_SOURCE_DIR}")

# The pointer lock (desktop_pointer.cc): XInput2's raw motion on X11, and the
# pointer-constraints and relative-pointer protocols on Wayland, whose client
# code wayland-scanner generates from wayland-protocols. A build without them
# holds no pointer on that display server; the viewer then leaves it free.
pkg_check_modules(XINPUT QUIET IMPORTED_TARGET xi x11)
if(XINPUT_FOUND)
  target_link_libraries(${BINARY_NAME} PRIVATE PkgConfig::XINPUT)
  target_compile_definitions(${BINARY_NAME} PRIVATE PB_MAPPER_LOCK_X11)
  message(STATUS "Pointer lock on X11: XInput2")
else()
  message(STATUS "Pointer lock on X11: libXi development files not found")
endif()
pkg_check_modules(WAYLAND_CLIENT QUIET IMPORTED_TARGET wayland-client)
pkg_check_modules(WAYLAND_PROTOCOLS QUIET wayland-protocols)
find_program(WAYLAND_SCANNER wayland-scanner)
if(WAYLAND_CLIENT_FOUND AND WAYLAND_PROTOCOLS_FOUND AND WAYLAND_SCANNER)
  enable_language(C)
  pkg_get_variable(WAYLAND_PROTOCOLS_DIR wayland-protocols pkgdatadir)
  set(LOCK_PROTOCOLS_DIR "${CMAKE_CURRENT_BINARY_DIR}/protocols")
  file(MAKE_DIRECTORY "${LOCK_PROTOCOLS_DIR}")
  foreach(protocol relative-pointer pointer-constraints)
    set(protocol_xml "${WAYLAND_PROTOCOLS_DIR}/unstable/${protocol}/${protocol}-unstable-v1.xml")
    set(protocol_header "${LOCK_PROTOCOLS_DIR}/${protocol}-unstable-v1-client-protocol.h")
    set(protocol_code "${LOCK_PROTOCOLS_DIR}/${protocol}-unstable-v1-protocol.c")
    add_custom_command(
      OUTPUT "${protocol_header}"
      COMMAND "${WAYLAND_SCANNER}" client-header "${protocol_xml}" "${protocol_header}"
      DEPENDS "${protocol_xml}"
      VERBATIM)
    add_custom_command(
      OUTPUT "${protocol_code}"
      COMMAND "${WAYLAND_SCANNER}" private-code "${protocol_xml}" "${protocol_code}"
      DEPENDS "${protocol_xml}"
      VERBATIM)
    # Generated code: the warnings the runner treats as errors are not ours.
    set_source_files_properties("${protocol_code}" PROPERTIES COMPILE_OPTIONS "-Wno-error")
    target_sources(${BINARY_NAME} PRIVATE "${protocol_header}" "${protocol_code}")
  endforeach()
  target_include_directories(${BINARY_NAME} PRIVATE "${LOCK_PROTOCOLS_DIR}")
  target_link_libraries(${BINARY_NAME} PRIVATE PkgConfig::WAYLAND_CLIENT)
  target_compile_definitions(${BINARY_NAME} PRIVATE PB_MAPPER_LOCK_WAYLAND)
  message(STATUS "Pointer lock on Wayland: pointer constraints and relative pointer")
else()
  message(STATUS "Pointer lock on Wayland: wayland-client, wayland-protocols or wayland-scanner not found")
endif()

# The native video decoder module (desktop_video_gst.cc), built where the
# GStreamer development files are installed. The runner loads it at run time,
# so the app needs neither the module nor GStreamer to start; without them
# every remote desktop session is JPEG. gstreamer-allocators gives a
# decoder's dma-buf descriptors. desktop_video_device.cc places each decoder
# on a GPU, from sysfs.
pkg_check_modules(GSTREAMER QUIET IMPORTED_TARGET
  gstreamer-1.0 gstreamer-app-1.0 gstreamer-video-1.0 gstreamer-allocators-1.0)
if(GSTREAMER_FOUND)
  find_package(Threads REQUIRED)
  add_library(pb_mapper_desktop_video MODULE "desktop_video_gst.cc" "desktop_video_device.cc")
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
