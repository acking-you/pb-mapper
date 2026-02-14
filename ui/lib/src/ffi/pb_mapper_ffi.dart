import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

/// Log callback: level 0..4, message pointer, timestamp seconds.
typedef LogCallbackNative =
    Void Function(Int32 level, Pointer<Utf8> message, Uint64 timestamp);

// FFI function signatures
typedef _PbMapperCreateNative = Pointer<Void> Function();
typedef _PbMapperCreateDart = Pointer<Void> Function();

typedef _PbMapperDestroyNative = Void Function(Pointer<Void> handle);
typedef _PbMapperDestroyDart = void Function(Pointer<Void> handle);

typedef _PbMapperSetAppDirNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> path);
typedef _PbMapperSetAppDirDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> path);

typedef _PbMapperStartServerNative =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Uint16 port,
      Int32 enableKeepAlive,
    );
typedef _PbMapperStartServerDart =
    Pointer<Utf8> Function(Pointer<Void> handle, int port, int enableKeepAlive);

typedef _PbMapperStopServerNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperStopServerDart = Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperRegisterServiceNative =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serviceKey,
      Pointer<Utf8> localAddress,
      Pointer<Utf8> protocol,
      Int32 enableEncryption,
      Int32 enableKeepAlive,
    );
typedef _PbMapperRegisterServiceDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serviceKey,
      Pointer<Utf8> localAddress,
      Pointer<Utf8> protocol,
      int enableEncryption,
      int enableKeepAlive,
    );

typedef _PbMapperUnregisterServiceNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);
typedef _PbMapperUnregisterServiceDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);

typedef _PbMapperDeleteServiceConfigNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);
typedef _PbMapperDeleteServiceConfigDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);

typedef _PbMapperConnectServiceNative =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serviceKey,
      Pointer<Utf8> localAddress,
      Pointer<Utf8> protocol,
      Int32 enableKeepAlive,
    );
typedef _PbMapperConnectServiceDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serviceKey,
      Pointer<Utf8> localAddress,
      Pointer<Utf8> protocol,
      int enableKeepAlive,
    );

typedef _PbMapperDisconnectServiceNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);
typedef _PbMapperDisconnectServiceDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);

typedef _PbMapperDeleteClientConfigNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);
typedef _PbMapperDeleteClientConfigDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);

typedef _PbMapperGetConfigNative = Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperGetConfigDart = Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperUpdateConfigNative =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serverAddress,
      Int32 enableKeepAlive,
      Pointer<Utf8> msgHeaderKey,
    );
typedef _PbMapperUpdateConfigDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serverAddress,
      int enableKeepAlive,
      Pointer<Utf8> msgHeaderKey,
    );

typedef _PbMapperGetServiceConfigsNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperGetServiceConfigsDart =
    Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperGetServiceStatusNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);
typedef _PbMapperGetServiceStatusDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);

typedef _PbMapperGetClientConfigsNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperGetClientConfigsDart =
    Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperGetClientStatusNative =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);
typedef _PbMapperGetClientStatusDart =
    Pointer<Utf8> Function(Pointer<Void> handle, Pointer<Utf8> serviceKey);

typedef _PbMapperGetLocalServerStatusNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperGetLocalServerStatusDart =
    Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperGetServerStatusDetailNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperGetServerStatusDetailDart =
    Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperSetLogCallbackNative =
    Void Function(Pointer<NativeFunction<LogCallbackNative>> callback);
typedef _PbMapperSetLogCallbackDart =
    void Function(Pointer<NativeFunction<LogCallbackNative>> callback);

typedef _PbMapperInitLoggingNative = Void Function();
typedef _PbMapperInitLoggingDart = void Function();

typedef _PbMapperFreeStringNative = Void Function(Pointer<Utf8> s);
typedef _PbMapperFreeStringDart = void Function(Pointer<Utf8> s);

class PbMapperFFI {
  static PbMapperFFI? _instance;
  static DynamicLibrary? _lib;

  PbMapperFFI._();

  factory PbMapperFFI() {
    _instance ??= PbMapperFFI._();
    return _instance!;
  }

  DynamicLibrary get lib {
    _lib ??= _loadLibrary();
    return _lib!;
  }

  static DynamicLibrary _loadLibrary() {
    if (Platform.isAndroid) {
      return DynamicLibrary.open('libpb_mapper_ffi.so');
    } else if (Platform.isIOS) {
      return DynamicLibrary.process();
    } else if (Platform.isMacOS) {
      try {
        return DynamicLibrary.open(
          '@executable_path/../Frameworks/libpb_mapper_ffi.dylib',
        );
      } catch (_) {
        return DynamicLibrary.open('libpb_mapper_ffi.dylib');
      }
    } else if (Platform.isWindows) {
      return DynamicLibrary.open('pb_mapper_ffi.dll');
    } else if (Platform.isLinux) {
      try {
        return DynamicLibrary.open('lib/libpb_mapper_ffi.so');
      } catch (_) {
        return DynamicLibrary.open('libpb_mapper_ffi.so');
      }
    }
    throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');
  }

  late final pbMapperCreate = lib
      .lookupFunction<_PbMapperCreateNative, _PbMapperCreateDart>(
        'pb_mapper_create',
      );

  late final pbMapperDestroy = lib
      .lookupFunction<_PbMapperDestroyNative, _PbMapperDestroyDart>(
        'pb_mapper_destroy',
      );

  late final pbMapperSetAppDir = lib
      .lookupFunction<_PbMapperSetAppDirNative, _PbMapperSetAppDirDart>(
        'pb_mapper_set_app_dir',
      );

  late final pbMapperStartServer = lib
      .lookupFunction<_PbMapperStartServerNative, _PbMapperStartServerDart>(
        'pb_mapper_start_server',
      );

  late final pbMapperStopServer = lib
      .lookupFunction<_PbMapperStopServerNative, _PbMapperStopServerDart>(
        'pb_mapper_stop_server',
      );

  late final pbMapperRegisterService = lib
      .lookupFunction<
        _PbMapperRegisterServiceNative,
        _PbMapperRegisterServiceDart
      >('pb_mapper_register_service');

  late final pbMapperUnregisterService = lib
      .lookupFunction<
        _PbMapperUnregisterServiceNative,
        _PbMapperUnregisterServiceDart
      >('pb_mapper_unregister_service');

  late final pbMapperDeleteServiceConfig = lib
      .lookupFunction<
        _PbMapperDeleteServiceConfigNative,
        _PbMapperDeleteServiceConfigDart
      >('pb_mapper_delete_service_config');

  late final pbMapperConnectService = lib
      .lookupFunction<
        _PbMapperConnectServiceNative,
        _PbMapperConnectServiceDart
      >('pb_mapper_connect_service');

  late final pbMapperDisconnectService = lib
      .lookupFunction<
        _PbMapperDisconnectServiceNative,
        _PbMapperDisconnectServiceDart
      >('pb_mapper_disconnect_service');

  late final pbMapperDeleteClientConfig = lib
      .lookupFunction<
        _PbMapperDeleteClientConfigNative,
        _PbMapperDeleteClientConfigDart
      >('pb_mapper_delete_client_config');

  late final pbMapperGetConfig = lib
      .lookupFunction<_PbMapperGetConfigNative, _PbMapperGetConfigDart>(
        'pb_mapper_get_config_json',
      );

  late final pbMapperUpdateConfig = lib
      .lookupFunction<_PbMapperUpdateConfigNative, _PbMapperUpdateConfigDart>(
        'pb_mapper_update_config',
      );

  late final pbMapperGetServiceConfigs = lib
      .lookupFunction<
        _PbMapperGetServiceConfigsNative,
        _PbMapperGetServiceConfigsDart
      >('pb_mapper_get_service_configs_json');

  late final pbMapperGetServiceStatus = lib
      .lookupFunction<
        _PbMapperGetServiceStatusNative,
        _PbMapperGetServiceStatusDart
      >('pb_mapper_get_service_status_json');

  late final pbMapperGetClientConfigs = lib
      .lookupFunction<
        _PbMapperGetClientConfigsNative,
        _PbMapperGetClientConfigsDart
      >('pb_mapper_get_client_configs_json');

  late final pbMapperGetClientStatus = lib
      .lookupFunction<
        _PbMapperGetClientStatusNative,
        _PbMapperGetClientStatusDart
      >('pb_mapper_get_client_status_json');

  late final pbMapperGetLocalServerStatus = lib
      .lookupFunction<
        _PbMapperGetLocalServerStatusNative,
        _PbMapperGetLocalServerStatusDart
      >('pb_mapper_get_local_server_status_json');

  late final pbMapperGetServerStatusDetail = lib
      .lookupFunction<
        _PbMapperGetServerStatusDetailNative,
        _PbMapperGetServerStatusDetailDart
      >('pb_mapper_get_server_status_detail_json');

  late final pbMapperSetLogCallback = lib
      .lookupFunction<
        _PbMapperSetLogCallbackNative,
        _PbMapperSetLogCallbackDart
      >('pb_mapper_set_log_callback');

  late final pbMapperInitLogging = lib
      .lookupFunction<_PbMapperInitLoggingNative, _PbMapperInitLoggingDart>(
        'pb_mapper_init_logging',
      );

  late final pbMapperFreeString = lib
      .lookupFunction<_PbMapperFreeStringNative, _PbMapperFreeStringDart>(
        'pb_mapper_free_string',
      );
}
