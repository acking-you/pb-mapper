#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>

#include <cstdio>
#include <io.h>

#include "flutter_window.h"
#include "utils.h"

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // Attach to console when present (e.g., 'flutter run' or `pb_mapper_ui
  // status`) or create a new console when running with a debugger.
  //
  // Two different failures to avoid here, both measured rather than guessed.
  //
  // When the shell redirects (`pb_mapper_ui status > out.txt`) the std handles
  // are already valid and everything works. AttachConsole would *overwrite*
  // them with the console, silently sending the output to a window instead of
  // the file. So attach only when there is nothing there.
  //
  // When it does not redirect, a GUI-subsystem process starts with no stdout
  // at all, and AttachConsole is what supplies one. It populates the handles
  // Rust writes through but leaves the C runtime's stdout unbound, and that is
  // the one Dart resolves: without the freopen_s below, Dart reports
  // stdout.hasTerminal == true while every write fails with
  // ERROR_INVALID_HANDLE. CreateAndAttachConsole() already does this on its
  // own branch; the attach path needs it too.
  HANDLE existing_stdout = ::GetStdHandle(STD_OUTPUT_HANDLE);
  bool stdout_already_usable =
      existing_stdout != nullptr && existing_stdout != INVALID_HANDLE_VALUE &&
      ::GetFileType(existing_stdout) != FILE_TYPE_UNKNOWN;

  if (!stdout_already_usable) {
    if (::AttachConsole(ATTACH_PARENT_PROCESS)) {
      FILE *unused;
      freopen_s(&unused, "CONOUT$", "w", stdout);
      freopen_s(&unused, "CONOUT$", "w", stderr);
    } else if (::IsDebuggerPresent()) {
      CreateAndAttachConsole();
    }
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1280, 720);
  if (!window.Create(L"ui", origin, size)) {
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  ::CoUninitialize();
  return EXIT_SUCCESS;
}
