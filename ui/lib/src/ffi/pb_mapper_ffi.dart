import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

/// State-change callback: one JSON payload, freed with `pb_mapper_free_string`.
///
/// Invoked from Rust worker threads, so it must be bound with
/// `NativeCallable.listener`.
typedef ChangeCallbackNative = Void Function(Pointer<Utf8> payload);

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

typedef _PbMapperStartControlServerNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperStartControlServerDart =
    Pointer<Utf8> Function(Pointer<Void> handle);

/// argc/argv in, exit code out — or [kNotACommand] when this is a normal launch.
typedef _PbMapperCliMainNative =
    Int32 Function(Int32 argc, Pointer<Pointer<Utf8>> argv);
typedef _PbMapperCliMainDart =
    int Function(int argc, Pointer<Pointer<Utf8>> argv);

/// Returned by `pb_mapper_cli_main` when argv is not a command.
const int kNotACommand = -1;

/// `quicPort` 0 runs the relay without QUIC.
typedef _PbMapperStartServerNative =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Uint16 port,
      Int32 enableKeepAlive,
      Uint16 quicPort,
    );
typedef _PbMapperStartServerDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      int port,
      int enableKeepAlive,
      int quicPort,
    );

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
      // Nonzero lets connections reach it directly.
      Int32 allowDirect,
      // When to map the direct UDP port: 0 never, 1 after a failed punch, 2 always.
      Int32 portMapping,
    );
typedef _PbMapperRegisterServiceDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serviceKey,
      Pointer<Utf8> localAddress,
      Pointer<Utf8> protocol,
      int enableEncryption,
      int enableKeepAlive,
      int allowDirect,
      int portMapping,
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
      // The temporary credential whose service it is; negative for none.
      Int64 namespace,
      // Nonzero tries a direct path before the relay.
      Int32 allowDirect,
      // When to map the direct UDP port: 0 never, 1 after a failed punch, 2 always.
      Int32 portMapping,
    );
typedef _PbMapperConnectServiceDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serviceKey,
      Pointer<Utf8> localAddress,
      Pointer<Utf8> protocol,
      int enableKeepAlive,
      int namespace,
      int allowDirect,
      int portMapping,
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

/// `relayTransport` is `tcp` or `quic`.
typedef _PbMapperUpdateConfigNative =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serverAddress,
      Int32 enableKeepAlive,
      Pointer<Utf8> msgHeaderKey,
      Pointer<Utf8> relayTransport,
    );
typedef _PbMapperUpdateConfigDart =
    Pointer<Utf8> Function(
      Pointer<Void> handle,
      Pointer<Utf8> serverAddress,
      int enableKeepAlive,
      Pointer<Utf8> msgHeaderKey,
      Pointer<Utf8> relayTransport,
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

typedef _PbMapperForceRefreshServerStatusNative =
    Pointer<Utf8> Function(Pointer<Void> handle);
typedef _PbMapperForceRefreshServerStatusDart =
    Pointer<Utf8> Function(Pointer<Void> handle);

typedef _PbMapperSetLogCallbackNative =
    Void Function(Pointer<NativeFunction<LogCallbackNative>> callback);
typedef _PbMapperSetLogCallbackDart =
    void Function(Pointer<NativeFunction<LogCallbackNative>> callback);

typedef _PbMapperInitLoggingNative = Void Function();
typedef _PbMapperInitLoggingDart = void Function();

typedef _PbMapperFreeStringNative = Void Function(Pointer<Utf8> s);
typedef _PbMapperFreeStringDart = void Function(Pointer<Utf8> s);

/// `PbDesktopViewV1`: what the viewer shows this frame, filled by the
/// per-frame leaf `pb_mapper_desktop_view_v1`. 56 bytes, native layout.
final class PbDesktopViewV1 extends Struct {
  @Uint64()
  external int generation;

  /// 0 = none.
  @Uint64()
  external int presentedFrameId;

  /// Increments per new cursor.
  @Uint64()
  external int cursorSeq;

  /// `UINT32_MAX` = none.
  @Uint32()
  external int presentedAgeMs;

  /// Physical pixels of `generation`: the input space.
  @Uint32()
  external int displayWidth;
  @Uint32()
  external int displayHeight;

  /// The encoded size.
  @Uint32()
  external int videoWidth;
  @Uint32()
  external int videoHeight;

  /// Physical pixels relative to the display.
  @Int32()
  external int cursorX;
  @Int32()
  external int cursorY;

  /// 0 none, 1 connecting, 2 streaming, 3 capture unavailable, 4 paused,
  /// 5 stopped.
  @Uint8()
  external int state;

  /// Bit 0 cursor visible, 1 controlling, 2 control allowed, 3 secure
  /// desktop, 4 fresh, 5 paused.
  @Uint8()
  external int flags;

  @Array(2)
  external Array<Uint8> reserved;
}

/// `PbDesktopVideoMetaV1`: what `pb_mapper_desktop_video_next_v1` says about
/// the unit it copied. 56 bytes.
final class PbDesktopVideoMetaV1 extends Struct {
  @Uint64()
  external int generation;
  @Uint64()
  external int frameId;

  /// The transport sequence, for trace correlation.
  @Uint64()
  external int seq;
  @Uint64()
  external int ptsUs;

  /// Since network receipt, measured at the pop.
  @Uint64()
  external int ageUs;
  @Uint32()
  external int width;
  @Uint32()
  external int height;

  /// Bytes written; on `NEED_CAPACITY` the bytes needed.
  @Uint32()
  external int len;

  /// Bit 0 keyframe, 1 discontinuity, 2 JPEG (else H.264), 3 duplicate.
  @Uint32()
  external int flags;
}

/// The return codes every video export shares (and the JNI functions).
abstract final class PbVideo {
  static const unit = 1;
  static const timeout = 0;
  static const closed = -1;
  static const needCapacity = -2;
  static const paused = -3;
  static const notAttached = -4;
  static const invalid = -5;
  static const alreadyAttached = -6;
}

class PbMapperFFI {
  late final pbMapperDesktopCall = lib
      .lookupFunction<
        Pointer<Utf8> Function(Pointer<Void>, Pointer<Utf8>),
        Pointer<Utf8> Function(Pointer<Void>, Pointer<Utf8>)
      >('pb_mapper_desktop_call_json');

  /// The viewer's hot ops. Synchronous and never blocks: safe on the UI
  /// isolate.
  late final pbMapperDesktopViewerCall = lib
      .lookupFunction<
        Pointer<Utf8> Function(Pointer<Void>, Pointer<Utf8>),
        Pointer<Utf8> Function(Pointer<Void>, Pointer<Utf8>)
      >('pb_mapper_desktop_viewer_call_json');

  /// 1 a viewer, 0 none (zeroed), -1 a null pointer or a caught panic. A
  /// leaf for every rendered frame; the UI isolate only.
  late final pbMapperDesktopView = lib
      .lookupFunction<
        Int32 Function(Pointer<Void>, Pointer<PbDesktopViewV1>),
        int Function(Pointer<Void>, Pointer<PbDesktopViewV1>)
      >('pb_mapper_desktop_view_v1', isLeaf: true);

  /// `> 0` the consumer id; otherwise `CLOSED` or `ALREADY_ATTACHED`.
  late final pbMapperDesktopVideoAttach = lib
      .lookupFunction<Int64 Function(Uint64), int Function(int)>(
        'pb_mapper_desktop_video_attach_v1',
        isLeaf: true,
      );

  /// Blocks up to the timeout: never on the UI isolate.
  late final pbMapperDesktopVideoNext = lib
      .lookupFunction<
        Int32 Function(
          Uint64,
          Uint32,
          Pointer<Uint8>,
          Uint32,
          Pointer<PbDesktopVideoMetaV1>,
        ),
        int Function(
          int,
          int,
          Pointer<Uint8>,
          int,
          Pointer<PbDesktopVideoMetaV1>,
        )
      >('pb_mapper_desktop_video_next_v1');

  /// 1 accepted, 0 rejected, or `CLOSED`. Non-blocking.
  late final pbMapperDesktopVideoPresented = lib
      .lookupFunction<
        Int32 Function(Uint64, Uint64, Uint64),
        int Function(int, int, int)
      >('pb_mapper_desktop_video_presented_v1', isLeaf: true);

  /// Idempotent; wakes a `next` blocked on the consumer.
  late final pbMapperDesktopVideoDetach = lib
      .lookupFunction<Void Function(Uint64), void Function(int)>(
        'pb_mapper_desktop_video_detach_v1',
        isLeaf: true,
      );

  /// The address of a static: native code that reached the same copy of the
  /// library reads the same value (D9).
  late final pbMapperDesktopInstanceToken = lib
      .lookupFunction<Uint64 Function(), int Function()>(
        'pb_mapper_desktop_instance_token_v1',
        isLeaf: true,
      );
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

  late final pbMapperStartControlServer = lib
      .lookupFunction<
        _PbMapperStartControlServerNative,
        _PbMapperStartControlServerDart
      >('pb_mapper_start_control_server');

  late final pbMapperSetChangeCallback = lib
      .lookupFunction<
        Void Function(Pointer<NativeFunction<ChangeCallbackNative>>),
        void Function(Pointer<NativeFunction<ChangeCallbackNative>>)
      >('pb_mapper_set_change_callback');

  late final pbMapperCliMain = lib
      .lookupFunction<_PbMapperCliMainNative, _PbMapperCliMainDart>(
        'pb_mapper_cli_main',
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

  late final pbMapperRevealIsolatedAdminKey = lib
      .lookupFunction<_PbMapperGetConfigNative, _PbMapperGetConfigDart>(
        'pb_mapper_reveal_isolated_admin_key',
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

  late final pbMapperGetServiceConns = lib
      .lookupFunction<
        _PbMapperGetServiceStatusNative,
        _PbMapperGetServiceStatusDart
      >('pb_mapper_get_service_conns_json');

  /// Every relay status and administrator operation, as `{"op": …}` JSON.
  late final pbMapperRelayCall = lib
      .lookupFunction<
        _PbMapperGetServiceStatusNative,
        _PbMapperGetServiceStatusDart
      >('pb_mapper_relay_call_json');

  /// A running tunnel's event readers, as `{"op": …}` JSON.
  late final pbMapperTunnelEventsCall = lib
      .lookupFunction<
        _PbMapperGetServiceStatusNative,
        _PbMapperGetServiceStatusDart
      >('pb_mapper_tunnel_events_call_json');

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

  late final pbMapperForceRefreshServerStatus = lib
      .lookupFunction<
        _PbMapperForceRefreshServerStatusNative,
        _PbMapperForceRefreshServerStatusDart
      >('pb_mapper_force_refresh_server_status_json');

  late final pbMapperSetLogCallback = lib
      .lookupFunction<
        _PbMapperSetLogCallbackNative,
        _PbMapperSetLogCallbackDart
      >('pb_mapper_set_log_callback');

  late final pbMapperInitLogging = lib
      .lookupFunction<_PbMapperInitLoggingNative, _PbMapperInitLoggingDart>(
        'pb_mapper_init_logging',
      );

  late final pbMapperShutdownLogging = lib
      .lookupFunction<Void Function(), void Function()>(
        'pb_mapper_shutdown_logging',
      );

  late final pbMapperFreeString = lib
      .lookupFunction<_PbMapperFreeStringNative, _PbMapperFreeStringDart>(
        'pb_mapper_free_string',
      );
}
