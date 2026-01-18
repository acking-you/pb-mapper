# ui

A new Flutter project.

## Getting Started

This project is a starting point for a Flutter application.

A few resources to get you started if this is your first Flutter project:

- [Lab: Write your first Flutter app](https://docs.flutter.dev/get-started/codelab)
- [Cookbook: Useful Flutter samples](https://docs.flutter.dev/cookbook)

For help getting started with Flutter development, view the
[online documentation](https://docs.flutter.dev/), which offers tutorials,
samples, guidance on mobile development, and a full API reference.

## Using Rust Inside Flutter

This project uses raw `dart:ffi` to call into a Rust FFI library (`pb-mapper-ffi`)
located under `ui/native`.

### Flutter UI Architecture

- **Views** live in `ui/lib/src/views` and represent pages:
  server management, service registration, client connection, status monitoring,
  and configuration.
- **Widgets** shared across pages live in `ui/lib/src/widgets`.
- **FFI layering**:
  - `pb_mapper_api.dart`: typed UI-facing API.
  - `pb_mapper_service.dart`: FFI dispatch + isolate execution (keeps UI thread
    responsive).
  - `pb_mapper_ffi.dart`: raw `dart:ffi` symbols and dynamic library loading.
  - `ui/native/pb_mapper_ffi`: Rust C ABI crate exposing JSON responses.
- **Logging**: Rust logs are pushed to Dart via `NativeCallable` and streamed
  through `PbMapperService.logStream`.

### Why the FFI API returns JSON

The Rust FFI layer returns a JSON string for every call (for example:
`{"success": true, "message": "...", "data": {...}}`). We intentionally keep
this JSON envelope because:

- **No generated bindings**: it avoids codegen and keeps the ABI stable even
  when Rust structs evolve.
- **Simple error handling**: every call can return `{success, message}` without
  maintaining extra error enums or parallel Dart structs.
- **Low maintenance**: adding new fields only affects the JSON payload, not the
  Dart FFI signatures.
- **Good enough performance**: UI interactions are dominated by network calls,
  so the JSON parse cost is negligible compared to keeping the API stable.

If a future workflow needs high-throughput data, we can introduce a typed C
struct or a binary format for that specific API, but the default remains JSON
for clarity and stability during development.

To run and build this app, you need to have
[Flutter SDK](https://docs.flutter.dev/get-started/install)
and [Rust toolchain](https://www.rust-lang.org/tools/install)
installed on your system.
You can check that your system is ready with the commands below.
Note that all the Flutter subcomponents should be installed.

```shell
rustc --version
flutter doctor
```

Build the Rust FFI library for your target platform and place the output under
`ui/native/<platform>/` (see platform build scripts and CI for the expected paths).

Now you can run and build this app just like any other Flutter projects.

```shell
flutter run
```

For detailed instructions on using FFI with Flutter, see the Flutter docs.
