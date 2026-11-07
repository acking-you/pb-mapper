import 'dart:async';
import 'dart:io';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import '../ffi/desktop_api.dart';
import '../ffi/desktop_video.dart';
import 'desktop_input.dart';

export '../ffi/desktop_api.dart'
    show DesktopCallException, DesktopInvitationFormat, DesktopInvitationInfo;
export 'desktop_input.dart';
export 'desktop_models.dart';

/// The most events one [DesktopStore.sendSequence] carries (`INPUT_BATCH_MAX`).
const kSequenceMax = 16;

const _backoffSeconds = [1, 2, 4, 8, 15];
const _protocolRetryLimit = 3;
const _noVideoReconnectMs = 5000;
const _statusVisible = Duration(milliseconds: 500);
const _statusHidden = Duration(seconds: 1);
const _hostWithPage = Duration(seconds: 1);
const _hostWhileSharing = Duration(seconds: 2);
const _viewTick = Duration(milliseconds: 33);
const _moveInterval = Duration(milliseconds: 4);
const _snapshotBound = Duration(milliseconds: 300);
const _controlRetry = Duration(seconds: 1);
const _statsInterval = Duration(seconds: 1);

/// Stop reasons that reconnect with backoff; `protocol` does too, a bounded
/// number of times in a row.
const _retryableStops = {
  'transport',
  'timeout',
  'capture_failed',
  'restarting',
};

/// Held while its owner needs the store's timers; [dispose] gives it back.
final class DesktopDemandHandle {
  DesktopDemandHandle._(this._release);
  void Function()? _release;

  void dispose() {
    final release = _release;
    _release = null;
    release?.call();
  }
}

/// A notifier that tells its owner when it gains its first listener or loses
/// its last, so sampling runs only while something shows the value.
class _SampledNotifier<T> extends ValueNotifier<T> {
  _SampledNotifier(super.value, this._listenersChanged);
  final void Function() _listenersChanged;

  bool get listened => hasListeners;

  @override
  void addListener(VoidCallback listener) {
    final first = !hasListeners;
    super.addListener(listener);
    if (first) _listenersChanged();
  }

  @override
  void removeListener(VoidCallback listener) {
    super.removeListener(listener);
    if (!hasListeners) _listenersChanged();
  }
}

/// Owns the desktop host and the viewer session, independently of
/// navigation. It is not a `ChangeNotifier`: each concern is its own
/// [ValueListenable], which changes only when its value does (D14). Nothing
/// polls while nothing needs it.
class DesktopStore {
  /// [now] dates the scheduled retry ([DesktopSessionView.retryAt]);
  /// [onRestartReady] is called once the helper's hand-off is ready and the
  /// app must restart into it.
  DesktopStore(
    this._api, {
    DesktopTextureFactory? textures,
    DateTime Function()? now,
    void Function()? onRestartReady,
  }) : _textures = textures ?? createDesktopVideoTexture,
       _now = now ?? DateTime.now,
       _onRestartReady = onRestartReady;

  final DesktopApiClient _api;
  final DesktopTextureFactory _textures;
  final DateTime Function() _now;
  final void Function()? _onRestartReady;

  bool _disposed = false;

  // ---------------------------------------------------------------- slices

  final _hostView = ValueNotifier(const DesktopHostView());
  final _session = ValueNotifier(const DesktopSessionView());
  final _geometry = ValueNotifier<DesktopGeometry?>(null);
  final _picture = ValueNotifier<DesktopPicture?>(null);
  final _jpegImage = ValueNotifier<ui.Image?>(null);
  late final _stats = _SampledNotifier(DesktopStats.none, _schedulePolling);
  final _quality = ValueNotifier(DesktopQuality.balanced);

  ValueListenable<DesktopHostView> get hostView => _hostView;
  ValueListenable<DesktopSessionView> get session => _session;

  /// Generation, display and video size; never per frame.
  ValueListenable<DesktopGeometry?> get geometry => _geometry;

  /// The native texture, which survives reconnects, or the JPEG picture.
  ValueListenable<DesktopPicture?> get picture => _picture;

  /// The current picture on platforms without a native decoder.
  ValueListenable<ui.Image?> get jpegImage => _jpegImage;

  /// At most once a second, and only while something listens.
  ValueListenable<DesktopStats> get stats => _stats;
  ValueListenable<DesktopQuality> get quality => _quality;

  /// The per-frame leaf (cursor, freshness, flags), as last read.
  DesktopView? readView() => _view;

  // ---------------------------------------------------------------- demand

  int _pageDemand = 0, _viewerDemand = 0;
  AppLifecycleListener? _lifecycle;
  AppLifecycleState? _lifecycleState;
  bool _wasVisible = false;

  /// The stores that read the view leaf every rendered frame. Persistent
  /// frame callbacks cannot be removed, so one serves them all.
  static final _framed = <DesktopStore>{};
  static bool _frameHooked = false;

  /// [DesktopDemand.page] polls the host every second; [DesktopDemand.viewer]
  /// is a viewer on screen: the session resumes while it is held and the
  /// app is visible, and pauses otherwise (D13).
  ///
  /// Widgets take and give back demand in `initState` and `dispose`, inside
  /// a frame, so the session's change is published after that frame.
  DesktopDemandHandle demand(DesktopDemand demand) {
    if (_disposed) return DesktopDemandHandle._(null);
    switch (demand) {
      case DesktopDemand.page:
        _pageDemand++;
        unawaited(_onPageDemand());
      case DesktopDemand.viewer:
        _viewerDemand++;
        _hookViewer();
    }
    scheduleMicrotask(_visibilityChanged);
    return DesktopDemandHandle._(() {
      if (_disposed) return;
      switch (demand) {
        case DesktopDemand.page:
          _pageDemand--;
        case DesktopDemand.viewer:
          _viewerDemand--;
      }
      scheduleMicrotask(_visibilityChanged);
    });
  }

  void _hookViewer() {
    if (_lifecycle == null) {
      _lifecycleState = WidgetsBinding.instance.lifecycleState;
      _lifecycle = AppLifecycleListener(
        onStateChange: (state) {
          _lifecycleState = state;
          _visibilityChanged();
        },
      );
    }
  }

  static void _onFrame(Duration _) {
    for (final store in _framed.toList()) {
      store._acceptView(store._readLeaf(), inFrame: true);
    }
  }

  bool get _visible =>
      _viewerDemand > 0 &&
      switch (_lifecycleState) {
        null || AppLifecycleState.resumed || AppLifecycleState.inactive => true,
        _ => false,
      };

  void _visibilityChanged() {
    if (_disposed) return;
    final visible = _visible;
    if (visible != _wasVisible) {
      _wasVisible = visible;
      visible ? _onVisible() : _onHidden();
    }
    _schedulePolling();
    _publishSession();
  }

  // ---------------------------------------------------------------- timers

  Timer? _hostTimer, _statusTimer, _viewTimer, _statsTimer;
  Duration? _hostEvery, _statusEvery;

  bool get _sessionActive =>
      _target != null && _end == null && !_identityChanged;
  bool get _viewPolling => _visible && _sessionActive;

  void _schedulePolling() {
    final hostEvery = _disposed || _restarting
        ? null
        : _pageDemand > 0
        ? _hostWithPage
        : _hostView.value.sharing
        ? _hostWhileSharing
        : null;
    if (hostEvery != _hostEvery) {
      _hostEvery = hostEvery;
      _hostTimer?.cancel();
      _hostTimer = hostEvery == null
          ? null
          : Timer.periodic(hostEvery, (_) => unawaited(_readHost()));
    }
    final statusEvery = _disposed || !_sessionActive
        ? null
        : _visible
        ? _statusVisible
        : _statusHidden;
    if (statusEvery != _statusEvery) {
      _statusEvery = statusEvery;
      _statusTimer?.cancel();
      _statusTimer = statusEvery == null
          ? null
          : Timer.periodic(statusEvery, (_) => _pollStatus());
    }
    final viewOn = !_disposed && _viewPolling;
    if (viewOn != (_viewTimer != null)) {
      _viewTimer?.cancel();
      _viewTimer = viewOn
          ? Timer.periodic(
              _viewTick,
              (_) => _acceptView(_readLeaf(), inFrame: false),
            )
          : null;
      if (viewOn) {
        _framed.add(this);
        if (!_frameHooked) {
          _frameHooked = true;
          SchedulerBinding.instance.addPersistentFrameCallback(_onFrame);
        }
      } else {
        _framed.remove(this);
      }
    }
    final statsOn = !_disposed && _sessionActive && _stats.listened;
    if (statsOn != (_statsTimer != null)) {
      _statsTimer?.cancel();
      _statsTimer = statsOn
          ? Timer.periodic(_statsInterval, (_) => _statsDue = true)
          : null;
      // A new listener sees the next status read, not a second later.
      _statsDue = statsOn;
    }
  }

  // ================================================================= host

  DesktopCapabilities? _capabilities;
  DesktopTrust? _trust;
  DesktopHostSnapshot? _host;
  bool _hostStatusRead = false, _hostBusy = false;
  bool _enabling = false, _restarting = false, _startupRestored = false;
  String? _hostError;
  int _hostSerial = 0, _hostResumeAttempt = 0;
  int _hostReads = 0, _hostApplied = 0;
  Timer? _hostResumeTimer;
  Future<void>? _hostLoading;

  bool _hostCurrent(int serial) => !_disposed && serial == _hostSerial;

  Future<void> _onPageDemand() async {
    if (_hostView.value.loaded) {
      try {
        _capabilities = await _api.capabilities();
      } catch (_) {
        // Keep the last answer; the helper row may lag one page visit.
      }
      await _readHost();
    } else {
      await _loadHost();
    }
  }

  Future<void> _loadHost() => _hostLoading ??= () async {
    try {
      _capabilities = await _api.capabilities();
      _trust = await _api.trustStatus();
      await _discoverBackground();
      await _readHost();
    } catch (e) {
      if (!_disposed) _hostError = e.toString();
      _publishHost();
    } finally {
      _hostLoading = null;
    }
  }();

  /// `host_status` reads the cached background state; this asks the
  /// service itself, which only the startup and explicit actions do.
  Future<void> _discoverBackground() async {
    final background = await _api.backgroundAction('status');
    if (_disposed) return;
    _host = DesktopHostSnapshot(host: _host?.host, background: background);
  }

  Future<void> _readHost() async {
    if (_disposed) return;
    final read = ++_hostReads;
    try {
      final snapshot = await _api.hostStatus();
      if (_disposed || read <= _hostApplied) return;
      _hostApplied = read;
      _host = snapshot;
      _hostStatusRead = true;
    } catch (_) {
      // A failed poll keeps the last state; actions report their own errors.
    }
    _publishHost();
  }

  Future<void> _readTrustAndHost(int serial) async {
    final trust = await _api.trustStatus();
    if (!_hostCurrent(serial)) return;
    _trust = trust;
    await _readHost();
  }

  void _publishHost() {
    if (_disposed) return;
    final capabilities = _capabilities;
    final trust = _trust;
    final host = _host?.host;
    final background = _host?.background;
    final backgroundHost = background?.host;
    final configuredBackground = background?.configured == true;
    final saved = trust?.saved;
    final matches = trust?.matchesRelay == true;
    _hostView.value = DesktopHostView(
      loaded: capabilities != null && trust != null && _hostStatusRead,
      supported: capabilities?.host == true,
      sharing: host != null || background?.enabled == true,
      configured: (saved != null && matches) || configuredBackground,
      enabling: _enabling,
      restarting: _restarting,
      background: configuredBackground,
      controlAllowed:
          host?.controlAllowed ??
          backgroundHost?.controlAllowed ??
          (configuredBackground
              ? background!.allowControl
              : saved?.allowControl) ??
          false,
      service:
          host?.service ??
          (configuredBackground ? background!.service : null) ??
          saved?.service,
      // No invitation of the store's own: the running host's, else the
      // background service's, else the remembered grant for this relay.
      invitation:
          host?.invitation ??
          (configuredBackground ? background!.invitation : null) ??
          (matches ? saved?.invitation : null),
      error: _hostError,
      displays: capabilities?.displays ?? const [],
      displayId:
          host?.display?.id ??
          backgroundHost?.display?.id ??
          (configuredBackground ? background!.displayId : null) ??
          saved?.displayId,
      helper: _helperState(capabilities?.helper),
      viewer: host?.viewer ?? backgroundHost?.viewer,
    );
    _schedulePolling();
  }

  static DesktopHelperState _helperState(DesktopHelper? helper) {
    if (helper == null || !helper.installed) return DesktopHelperState.none;
    if (helper.updateRequired) return DesktopHelperState.outdated;
    if (!helper.running) return DesktopHelperState.stopped;
    if (!helper.authorized || !helper.identityVerified) {
      return DesktopHelperState.unverified;
    }
    return DesktopHelperState.ready;
  }

  /// One host action at a time; a second one while it runs is ignored.
  Future<bool> _hostAction(
    Future<void> Function() action, {
    bool enabling = false,
  }) async {
    if (_hostBusy || _disposed) return false;
    _hostBusy = true;
    _enabling = enabling;
    _hostError = null;
    _publishHost();
    try {
      await action();
      return true;
    } catch (e) {
      if (!_disposed) _hostError = e.toString();
      return false;
    } finally {
      _hostBusy = false;
      _enabling = false;
      _publishHost();
    }
  }

  void _restart() {
    final restart = _onRestartReady;
    if (restart == null) {
      throw StateError('Application restart is unavailable');
    }
    _restarting = true;
    _publishHost();
    restart();
  }

  Future<void> restoreOnStartup() async {
    if (_startupRestored || _disposed) return;
    _startupRestored = true;
    await _restoreStartupHost(_hostSerial);
  }

  Future<void> _restoreStartupHost(int serial) async {
    if (!_hostCurrent(serial)) return;
    try {
      // A restarting helper must not prevent discovering the durable
      // hand-off that asks us to retry it. Read the local intent before
      // service IPC.
      final trust = await _api.trustStatus();
      if (!_hostCurrent(serial)) return;
      _trust = trust;
      if (trust.saved?.background == true &&
          trust.saved?.autoStart == true &&
          trust.matchesRelay) {
        await _resumePendingHost(serial);
        return;
      }
      _capabilities = await _api.capabilities();
      await _discoverBackground();
      await _readHost();
      if (!_hostCurrent(serial)) return;
      if (_host?.background?.configured != true &&
          trust.saved?.autoStart == true &&
          trust.matchesRelay) {
        await _hostAction(() async {
          await _api.restoreHost();
          await _readTrustAndHost(serial);
        });
      }
    } catch (e) {
      if (!_hostCurrent(serial)) return;
      _hostError = e.toString();
      _scheduleHostResume(serial, _restoreStartupHost);
    }
    _publishHost();
  }

  Future<void> _resumePendingHost(int serial) async {
    if (!_hostCurrent(serial)) return;
    try {
      final trust = await _api.trustStatus();
      if (!_hostCurrent(serial)) return;
      _trust = trust;
      if (trust.saved?.background != true ||
          trust.saved?.autoStart != true ||
          !trust.matchesRelay) {
        // A previous attempt may have configured the service and then lost
        // its reply. Discover that owner without enabling again.
        await _restoreStartupHost(serial);
        return;
      }
      await _api.restoreHost();
      if (!_hostCurrent(serial)) return;
      _hostResumeAttempt = 0;
      _hostError = null;
      _capabilities = await _api.capabilities();
      await _discoverBackground();
      if (!_hostCurrent(serial)) return;
      await _readTrustAndHost(serial);
    } catch (e) {
      if (!_hostCurrent(serial)) return;
      _hostError = e.toString();
      _scheduleHostResume(serial, _resumePendingHost);
    }
    _publishHost();
  }

  void _scheduleHostResume(int serial, Future<void> Function(int) retry) {
    if (!_hostCurrent(serial)) return;
    _hostResumeTimer?.cancel();
    _hostResumeTimer = Timer(
      Duration(seconds: (1 << _hostResumeAttempt.clamp(0, 4)).clamp(1, 15)),
      () {
        _hostResumeTimer = null;
        _hostResumeAttempt++;
        unawaited(retry(serial));
      },
    );
  }

  void _cancelHostResume() {
    _hostSerial++;
    _hostResumeTimer?.cancel();
    _hostResumeTimer = null;
    _hostResumeAttempt = 0;
  }

  /// Shares this desktop. By default through the Windows helper, so UAC and
  /// the lock screen work and sharing survives the app; [ordinaryOnly]
  /// shares the ordinary desktop from this app alone.
  Future<void> enableHost({
    required int displayId,
    required bool allowControl,
    String? service,
    bool ordinaryOnly = false,
  }) async {
    final name = service ?? _hostView.value.service ?? 'desktop:computer';
    await _hostAction(enabling: true, () async {
      _cancelHostResume();
      final serial = _hostSerial;
      if (ordinaryOnly) {
        await _api.startHost(
          service: name,
          displayId: displayId,
          allowControl: allowControl,
          remember: true,
          autoStart: true,
        );
      } else {
        final DesktopHostEnable result;
        try {
          result = await _api.enableHost(
            name,
            displayId,
            allowControl: allowControl,
          );
        } catch (_) {
          // A failed IPC reply can leave durable setup waiting for the
          // helper. Resume that intent through HostRestore, which never
          // installs or asks for UAC. A cancelled installation has already
          // retired it.
          try {
            final pending = await _api.trustStatus();
            if (_hostCurrent(serial) &&
                pending.saved?.background == true &&
                pending.saved?.autoStart == true &&
                pending.matchesRelay) {
              _trust = pending;
              _scheduleHostResume(serial, _resumePendingHost);
            }
          } catch (_) {
            // Keep the original setup error when local trust cannot be read.
          }
          rethrow;
        }
        if (!_hostCurrent(serial)) return;
        if (result.restart) {
          _restart();
          return;
        }
      }
      if (!_hostCurrent(serial)) return;
      await _readTrustAndHost(serial);
    });
  }

  /// Starts the remembered or configured host again.
  Future<void> resumeHost() async {
    await _hostAction(enabling: true, () async {
      _cancelHostResume();
      final serial = _hostSerial;
      if (_host?.background?.configured == true) {
        final background = await _api.backgroundAction('start');
        if (!_hostCurrent(serial)) return;
        _host = DesktopHostSnapshot(host: _host?.host, background: background);
      } else {
        await _api.restoreHost();
      }
      if (!_hostCurrent(serial)) return;
      await _readTrustAndHost(serial);
    });
  }

  /// Never waits for another host action: a local stop always works.
  Future<void> stopHost() async {
    if (_disposed) return;
    _cancelHostResume();
    final serial = _hostSerial;
    try {
      await _api.stopHost();
      if (!_hostCurrent(serial)) return;
      await _readTrustAndHost(serial);
    } catch (e) {
      if (_hostCurrent(serial)) _hostError = e.toString();
    }
    _publishHost();
  }

  /// Allowing control waits its turn; making the session view-only never
  /// does (D24).
  Future<void> allowControl(bool allow) async {
    if (allow) {
      await _hostAction(() async {
        await _api.allowControl(true);
        await _readHost();
      });
      return;
    }
    await _immediate(() => _api.allowControl(false));
  }

  /// Disconnects the viewer; hosting continues (D15). Never waits.
  Future<void> endViewerSession() => _immediate(_api.endViewerSession);

  Future<void> _immediate(Future<void> Function() action) async {
    if (_disposed) return;
    try {
      await action();
    } catch (e) {
      if (!_disposed) _hostError = e.toString();
    }
    await _readHost();
  }

  Future<void> setHostDisplay(int displayId) async {
    await _hostAction(() async {
      final serial = _hostSerial;
      await _api.setHostDisplay(displayId);
      if (_hostCurrent(serial)) await _readTrustAndHost(serial);
    });
  }

  Future<void> renameHostService(String service) async {
    await _hostAction(() async {
      final serial = _hostSerial;
      await _api.renameHostService(service);
      if (_hostCurrent(serial)) await _readTrustAndHost(serial);
    });
  }

  Future<void> revokeTrust() async {
    if (_disposed) return;
    _cancelHostResume();
    final serial = _hostSerial;
    try {
      // Native revocation serialises with pending setup and checks the
      // actual service owner.
      await _api.revokeTrust();
      if (!_hostCurrent(serial)) return;
      _hostError = null;
      await _readTrustAndHost(serial);
    } catch (e) {
      if (_hostCurrent(serial)) _hostError = e.toString();
    }
    _publishHost();
  }

  /// `install`, `disable` or `uninstall`. True when the app restarts into
  /// the installed helper's hand-off.
  Future<bool> helperAction(String action) async {
    var restart = false;
    await _hostAction(() async {
      if (await _api.helperAction(action)) {
        restart = true;
        _restart();
        return;
      }
      _capabilities = await _api.capabilities();
      await _discoverBackground();
      await _readHost();
    });
    return restart;
  }

  // ================================================================ viewer

  DesktopTarget? _target;

  /// A connect or a disconnect supersedes everything older.
  int _serial = 0;
  int? _attemptSerial;

  /// The connected Rust viewer's video; null while none is connected.
  DesktopSession? _media;
  DesktopViewerStatus? _status;
  DesktopView? _view;
  DesktopSetupStep? _setup;
  DesktopEndReason? _end;
  DesktopFailure? _failure;
  bool _identityChanged = false, _hasPicture = false, _stopHandled = false;
  bool _controlWanted = false,
      _controlSuppressed = false,
      _nativePaused = false;
  bool _statsDue = false;
  int _reconnectAttempt = 0, _protocolStops = 0;
  Timer? _retryTimer;
  DateTime? _retryAt;

  /// Running for a second after control was asked for; no second request
  /// goes out meanwhile.
  Timer? _controlCooldown;

  /// A stop seen while hidden, acted on when the viewer is visible again.
  String? _stoppedWhileHidden;
  Future<void>? _closing, _nativeDisconnect;

  Future<DesktopInvitationInfo> inspectInvitation(String text) =>
      _api.inspectInvitation(text);

  /// Connects [target], superseding whatever runs: an attempt in flight is
  /// cancelled, a pending retry cleared, and a disconnect already running is
  /// awaited first. Never gated by the host side.
  Future<void> connect(DesktopTarget target) async {
    if (_disposed) return;
    final serial = ++_serial;
    final previous = _target;
    final sameComputer =
        previous != null &&
        previous.invitation == target.invitation &&
        previous.computerId == target.computerId;
    _cancelRetry();
    _target = target;
    _reconnectAttempt = 0;
    _protocolStops = 0;
    _end = null;
    _failure = null;
    _identityChanged = false;
    _stoppedWhileHidden = null;
    _controlWanted = true;
    _cancelControlCooldown();
    _quality.value = target.quality;
    if (!sameComputer) {
      // Another computer's last picture must not show under this one.
      _hasPicture = false;
      _releaseMedia();
    }
    _attemptSerial = serial;
    _setup = DesktopSetupStep.finding;
    _publishSession();
    _schedulePolling();
    final closing = _closing;
    if (closing != null) await closing;
    if (serial != _serial) return;
    await _attempt(serial);
  }

  Future<void> _attempt(int serial) async {
    final target = _target;
    if (_disposed || target == null || serial != _serial) return;
    _attemptSerial = serial;
    _setup = DesktopSetupStep.finding;
    _status = null;
    _media = null;
    _invalidateInput();
    _publishSession();
    _schedulePolling();
    try {
      _cancelJpeg();
      // The FFI refuses a second viewer; this also cancels an older
      // attempt still waiting on the network.
      await _disconnectNative();
      if (_disposed || serial != _serial) return;
      final session = await _api.connect(
        target.invitation,
        computerId: target.computerId,
        relayOnly: target.relayOnly,
        quality: _quality.value,
        refreshHz: _refreshHz(),
      );
      if (_disposed || serial != _serial) return;
      _attemptSerial = null;
      _media = session;
      _stopHandled = false;
      _nativePaused = false;
      _controlSuppressed = false;
      _cancelControlCooldown();
      _setup = DesktopSetupStep.firstPicture;
      // A new Rust viewer: new sequences, so nothing older may be sent.
      _invalidateInput();
      _bumpGeometry();
      if (!_visible) _pauseNative();
      try {
        await _attachMedia(session, serial);
      } catch (e) {
        throw DesktopCallException('DECODER', e.toString());
      }
      if (_disposed || serial != _serial) return;
      _pollStatus();
    } on DesktopCallException catch (e) {
      if (_disposed || serial != _serial) return;
      _attemptSerial = null;
      _connectFailed(DesktopFailure(e.code, e.message));
    } catch (e) {
      if (_disposed || serial != _serial) return;
      _attemptSerial = null;
      _connectFailed(DesktopFailure('INTERNAL', e.toString()));
    } finally {
      if (_attemptSerial == serial) _attemptSerial = null;
      if (!_disposed && serial == _serial) {
        _publishSession();
        _schedulePolling();
      }
    }
  }

  void _connectFailed(DesktopFailure failure) {
    switch (failure.code) {
      case 'IO' || 'HOST_BUSY':
        _scheduleReconnect(failure);
      case 'IDENTITY_CHANGED':
        // The pinned identity is kept; only a deliberate new invitation
        // replaces it.
        _identityChanged = true;
        _endSession(null, failure);
      default:
        _endSession(_endReason(failure.code), failure);
    }
  }

  static DesktopEndReason _endReason(String code) => switch (code) {
    'host_stopped' => DesktopEndReason.hostStopped,
    'host_ended' => DesktopEndReason.disconnectedByHost,
    'revoked' => DesktopEndReason.revoked,
    'VERSION_MISMATCH' => DesktopEndReason.versionMismatch,
    'ACCESS_DENIED' => DesktopEndReason.denied,
    'INVALID_INVITATION' ||
    'SETUP_STRING' => DesktopEndReason.invalidInvitation,
    'decoder_failed' || 'DECODER' => DesktopEndReason.decoderFailed,
    _ => DesktopEndReason.failed,
  };

  /// Terminal: no automatic retry. The picture stays until a disconnect or
  /// a new connect.
  void _endSession(DesktopEndReason? reason, DesktopFailure failure) {
    _cancelRetry();
    _end = reason ?? (_identityChanged ? null : DesktopEndReason.failed);
    _failure = failure;
    _media = null;
    _stoppedWhileHidden = null;
    _invalidateInput();
    _cancelJpeg();
    unawaited(_disconnectNative());
    _publishSession();
    _schedulePolling();
  }

  void _scheduleReconnect(DesktopFailure failure, {bool immediate = false}) {
    if (_disposed || !_sessionActive) return;
    if (_retryTimer != null || _attemptSerial != null) return;
    _failure = failure;
    _invalidateInput();
    if (!_visible) {
      // Never reconnect a hidden viewer; resuming acts on this.
      _stoppedWhileHidden = failure.code;
      _publishSession();
      return;
    }
    _reconnectAttempt++;
    final serial = _serial;
    if (immediate) {
      unawaited(_attempt(serial));
      return;
    }
    final delay = Duration(
      seconds: _backoffSeconds[math.min(_reconnectAttempt - 1, 4)],
    );
    _retryAt = _now().add(delay);
    _retryTimer = Timer(delay, () {
      _retryTimer = null;
      _retryAt = null;
      unawaited(_attempt(serial));
    });
    _publishSession();
    _schedulePolling();
  }

  void _cancelRetry() {
    _retryTimer?.cancel();
    _retryTimer = null;
    _retryAt = null;
  }

  void _handleStop(String code, String message, {bool resumed = false}) {
    if (_stopHandled) return;
    _stopHandled = true;
    final failure = DesktopFailure(code, message);
    if (code == 'protocol' && ++_protocolStops > _protocolRetryLimit) {
      // A deterministic parse failure must not loop.
      _endSession(DesktopEndReason.failed, DesktopFailure('PROTOCOL', message));
    } else if (code == 'protocol' || _retryableStops.contains(code)) {
      // A frozen app finds its session closed by the host: reconnect at
      // once rather than after a backoff (R13).
      _scheduleReconnect(
        failure,
        immediate: resumed && (code == 'transport' || code == 'timeout'),
      );
    } else {
      _endSession(_endReason(code), failure);
    }
  }

  void _pollStatus({bool resumed = false}) {
    if (_disposed || !_sessionActive) return;
    DesktopViewerStatus? status;
    try {
      status = _api.viewerStatus();
    } catch (_) {
      return;
    }
    if (_attemptSerial != null) {
      if (status?.state == 'connecting') _setup = _setupFor(status!.stage);
      _publishSession();
      return;
    }
    if (_retryTimer != null || _media == null) {
      _publishSession();
      return;
    }
    _status = status;
    if (status == null) {
      _handleStop('transport', 'The viewer closed', resumed: resumed);
      _publishSession();
      return;
    }
    if (!status.controlling) _controlSuppressed = false;
    _applyGeometry(status);
    _quality.value = status.quality;
    switch (status.state) {
      case 'stopped':
        _handleStop(
          status.reason ?? 'transport',
          status.message,
          resumed: resumed,
        );
      case 'connecting':
        _setup = _setupFor(status.stage);
      case 'streaming':
        if (status.presentedFrameId == null) {
          _setup = DesktopSetupStep.firstPicture;
        } else {
          _hasPicture = true;
        }
        if (status.reason == 'input_failed') {
          // The host's input failed: do not win control back by itself.
          _controlWanted = false;
        }
        if (_phase() == DesktopPhase.live) {
          _reconnectAttempt = 0;
          _protocolStops = 0;
          _failure = null;
          _setup = null;
        }
        _maybeRequestControl(status);
        if (_visible &&
            !status.paused &&
            status.decoderState != 'failed' &&
            (status.lastVideoAgeMs ?? 0) >= _noVideoReconnectMs) {
          _scheduleReconnect(DesktopFailure('timeout', 'No video for 5 s'));
        }
    }
    _sampleStats(status);
    _publishSession();
  }

  static DesktopSetupStep _setupFor(String? stage) => switch (stage) {
    'identity' => DesktopSetupStep.verifying,
    'authorizing' => DesktopSetupStep.authorizing,
    _ => DesktopSetupStep.finding,
  };

  /// Control is asked for again only on a fresh picture, at most once a
  /// second, and never while hidden.
  void _maybeRequestControl(DesktopViewerStatus status) {
    if (!status.fresh) return;
    if (!status.controlAllowed) _controlWanted = false;
    if (!_visible ||
        !_controlWanted ||
        status.controlling ||
        _controlSuppressed ||
        _controlCooldown != null) {
      return;
    }
    _startControlCooldown();
    try {
      _api.control(true);
    } catch (_) {
      // Asked again after the cooldown.
    }
  }

  void _startControlCooldown() {
    _controlCooldown?.cancel();
    _controlCooldown = Timer(_controlRetry, () => _controlCooldown = null);
  }

  void _cancelControlCooldown() {
    _controlCooldown?.cancel();
    _controlCooldown = null;
  }

  /// At most once a second, while [stats] has listeners (D14).
  void _sampleStats(DesktopViewerStatus status) {
    if (!_statsDue || !_stats.listened) return;
    _statsDue = false;
    _stats.value = status.stats;
  }

  DesktopPhase _phase() {
    if (_target == null) return DesktopPhase.idle;
    if (_identityChanged) return DesktopPhase.identityChanged;
    if (_end != null) return DesktopPhase.ended;
    if (_retryTimer != null) return DesktopPhase.reconnecting;
    if (_attemptSerial != null) return DesktopPhase.connecting;
    if (!_visible) return DesktopPhase.paused;
    final status = _status;
    if (status == null || _media == null) return DesktopPhase.connecting;
    return switch (status.state) {
      'connecting' => DesktopPhase.connecting,
      'capture_unavailable' => DesktopPhase.waitingForDesktop,
      'paused' => DesktopPhase.paused,
      'stopped' => DesktopPhase.reconnecting,
      _ when status.presentedFrameId == null => DesktopPhase.connecting,
      _
          when status.decoderState == 'recovering' ||
              (!status.fresh &&
                  status.lastVideoAgeMs != null &&
                  status.lastVideoAgeMs! < 1000) =>
        DesktopPhase.recovering,
      _ => DesktopPhase.live,
    };
  }

  void _publishSession() {
    if (_disposed) return;
    final phase = _phase();
    final connected =
        _media != null && _attemptSerial == null && _retryTimer == null;
    final status = connected ? _status : null;
    final path = status?.stats.path;
    _session.value = DesktopSessionView(
      target: _target,
      phase: phase,
      setup: switch (phase) {
        DesktopPhase.connecting => _setup,
        DesktopPhase.ended ||
        DesktopPhase.identityChanged when !_hasPicture => _setup,
        _ => null,
      },
      reconnectAttempt: _reconnectAttempt,
      retryAt: _retryAt,
      end: _end,
      failure: _failure,
      controlAllowed: status?.controlAllowed ?? false,
      controlWanted: _controlWanted,
      controlling: (status?.controlling ?? false) && !_controlSuppressed,
      fresh: status?.fresh ?? false,
      secureDesktop: status?.secureDesktop ?? false,
      hasPicture: _hasPicture,
      paused: _target != null && (_nativePaused || !_visible),
      path: path == null || path.isEmpty ? null : path,
      notice: status?.notice,
      capabilities: status?.capabilities ?? const [],
    );
    if (_session.value.inputLive != _wasLive) {
      _wasLive = _session.value.inputLive;
      if (!_wasLive) _invalidateInput();
    }
  }

  bool _wasLive = false;

  // ---------------------------------------------------------------- view

  DesktopView? _readLeaf() {
    try {
      return _api.view();
    } catch (_) {
      return null;
    }
  }

  /// The leaf runs every rendered frame, and from a 33 ms timer that asks
  /// for a frame only when something a frame shows has changed. A change of
  /// generation, state or flags re-reads the status, which publishes it.
  void _acceptView(DesktopView? view, {required bool inFrame}) {
    final previous = _view;
    _view = view;
    if (view == null) return;
    final structural =
        previous == null ||
        previous.generation != view.generation ||
        previous.state != view.state ||
        previous.controlling != view.controlling ||
        previous.controlAllowed != view.controlAllowed ||
        previous.secureDesktop != view.secureDesktop ||
        previous.fresh != view.fresh ||
        previous.paused != view.paused ||
        previous.displayWidth != view.displayWidth ||
        previous.displayHeight != view.displayHeight ||
        previous.videoWidth != view.videoWidth ||
        previous.videoHeight != view.videoHeight ||
        (previous.presentedFrameId == 0) != (view.presentedFrameId == 0);
    if (structural) {
      if (inFrame) {
        SchedulerBinding.instance.addPostFrameCallback((_) => _pollStatus());
      } else {
        _pollStatus();
      }
    }
    if (!inFrame &&
        (structural ||
            previous.cursorSeq != view.cursorSeq ||
            previous.cursorVisible != view.cursorVisible)) {
      SchedulerBinding.instance.scheduleFrame();
    }
  }

  int _geometryEpoch = 0;

  void _applyGeometry(DesktopViewerStatus status) {
    final display = status.display;
    if (status.generation <= 0 || display == null) return;
    final current = _geometry.value;
    final moved =
        current == null ||
        current.generation != status.generation ||
        current.displayWidth != display.width ||
        current.displayHeight != display.height;
    final next = DesktopGeometry(
      epoch: moved ? ++_geometryEpoch : current.epoch,
      generation: status.generation,
      displayWidth: display.width,
      displayHeight: display.height,
      videoWidth: status.videoWidth > 0
          ? status.videoWidth
          : current?.videoWidth ?? display.width,
      videoHeight: status.videoHeight > 0
          ? status.videoHeight
          : current?.videoHeight ?? display.height,
    );
    if (next == current) return;
    if (moved) _invalidateInput();
    _geometry.value = next;
    _publishPicture();
  }

  /// A reconnect keeps the picture's size but no input from before it.
  void _bumpGeometry() {
    final current = _geometry.value;
    if (current == null) return;
    _geometry.value = DesktopGeometry(
      epoch: ++_geometryEpoch,
      generation: current.generation,
      displayWidth: current.displayWidth,
      displayHeight: current.displayHeight,
      videoWidth: current.videoWidth,
      videoHeight: current.videoHeight,
    );
  }

  // ------------------------------------------------------- pause, resume

  void _pauseNative() {
    if (_media == null || _nativePaused) return;
    _nativePaused = true;
    try {
      _api.pause();
    } catch (_) {
      // A stopped viewer needs no pause.
    }
  }

  void _onHidden() {
    _pauseNative();
    // The host revoked control when it paused; nothing queued survives.
    if (_media != null) _controlSuppressed = true;
    _invalidateInput();
    if (_retryTimer != null) {
      _cancelRetry();
      _stoppedWhileHidden = _failure?.code ?? 'transport';
    }
  }

  void _onVisible() {
    if (_media != null && _nativePaused) {
      _nativePaused = false;
      try {
        _api.resume();
      } catch (_) {
        // The status read below finds out why.
      }
    }
    final stopped = _stoppedWhileHidden;
    _stoppedWhileHidden = null;
    if (stopped != null) {
      _scheduleReconnect(
        _failure ?? DesktopFailure(stopped, ''),
        immediate: stopped == 'transport' || stopped == 'timeout',
      );
    } else {
      _pollStatus(resumed: true);
    }
  }

  // --------------------------------------------------------------- media

  DesktopVideoTexture? _texture;
  int _mediaSerial = 0;
  StreamSubscription<DesktopJpegFrame>? _jpeg;

  Future<void> _attachMedia(DesktopSession session, int serial) async {
    if (session.codec == 'jpeg') {
      _jpeg = _api
          .jpegFrames(session.videoSource)
          .listen(_onJpeg, onError: (Object _) {});
      return;
    }
    final media = _mediaSerial;
    var texture = _texture;
    if (texture == null) {
      final created = await _textures(instanceToken: _api.instanceToken());
      if (_disposed || media != _mediaSerial) {
        // Disconnected while the texture was created.
        unawaited(created.dispose().catchError((Object _) {}));
        return;
      }
      _texture = texture = created;
    }
    await texture.attach(session.videoSource);
    if (_disposed || serial != _serial) return;
    _publishPicture();
  }

  /// Stops consuming JPEG pictures. Nothing waits for the cancellation: the
  /// worker detaches once its source closes.
  void _cancelJpeg() {
    final jpeg = _jpeg;
    _jpeg = null;
    unawaited(jpeg?.cancel());
  }

  /// Drops the texture and picture: the next picture is another target's.
  void _releaseMedia() {
    _mediaSerial++;
    final texture = _texture;
    _texture = null;
    if (texture != null) {
      unawaited(texture.dispose().catchError((Object _) {}));
    }
    _disposeImageLater(_jpegImage.value);
    _jpegImage.value = null;
    _picture.value = null;
    _geometry.value = null;
  }

  void _publishPicture() {
    if (_disposed) return;
    final geometry = _geometry.value;
    final texture = _texture;
    final image = _jpegImage.value;
    if (texture != null) {
      final width = geometry?.videoWidth ?? 16;
      final height = geometry?.videoHeight ?? 9;
      _picture.value = DesktopPicture(
        textureId: texture.textureId,
        aspectRatio: width > 1 && height > 1 ? width / height : 16 / 9,
        codec: 'h264',
      );
    } else if (image != null) {
      _picture.value = DesktopPicture(
        aspectRatio: image.width / image.height,
        codec: 'jpeg',
      );
    }
  }

  void _onJpeg(DesktopJpegFrame frame) {
    final subscription = _jpeg;
    if (subscription == null) return;
    // One picture at a time: the next is taken after this one is presented.
    subscription.pause();
    unawaited(_presentJpeg(frame, subscription, _serial));
  }

  Future<void> _presentJpeg(
    DesktopJpegFrame frame,
    StreamSubscription<DesktopJpegFrame> subscription,
    int serial,
  ) async {
    ui.Image? image;
    try {
      final buffer = await ui.ImmutableBuffer.fromUint8List(frame.bytes);
      final codec = await ui.instantiateImageCodecWithSize(
        buffer,
        getTargetSize: (width, height) {
          final scale = math.min(1.0, math.min(1600 / width, 1000 / height));
          return ui.TargetImageSize(
            width: math.max(1, (width * scale).round()),
            height: math.max(1, (height * scale).round()),
          );
        },
      );
      final info = await codec.getNextFrame();
      codec.dispose();
      image = info.image;
    } catch (_) {
      // A picture that does not decode is skipped; Rust's recovery asks the
      // host for a keyframe.
    }
    final current =
        !_disposed && serial == _serial && identical(_jpeg, subscription);
    if (!current || image == null) {
      image?.dispose();
      if (current) subscription.resume();
      return;
    }
    _disposeImageLater(_jpegImage.value);
    _jpegImage.value = image;
    _publishPicture();
    SchedulerBinding.instance.addPostFrameCallback((_) {
      if (_disposed || serial != _serial || !identical(_jpeg, subscription)) {
        return;
      }
      try {
        _api.presentedJpeg(frame.consumer, frame.generation, frame.frameId);
      } catch (_) {
        // A closed consumer ends with its session.
      }
      _hasPicture = true;
      _publishSession();
      subscription.resume();
    });
    SchedulerBinding.instance.scheduleFrame();
  }

  void _disposeImageLater(ui.Image? image) {
    if (image == null) return;
    // RawImage holds its own clone; this handle can go after the frame.
    SchedulerBinding.instance.addPostFrameCallback((_) => image.dispose());
  }

  // ----------------------------------------------------------- lifecycle

  Future<void> _disconnectNative() {
    final running = _nativeDisconnect;
    if (running != null) return running;
    late final Future<void> disconnecting;
    disconnecting = _api.disconnect().catchError((Object _) {}).whenComplete(
      () {
        if (identical(_nativeDisconnect, disconnecting)) {
          _nativeDisconnect = null;
        }
      },
    );
    return _nativeDisconnect = disconnecting;
  }

  /// Ends the session. With [snapshotPath] and a fresh picture, it first
  /// writes a thumbnail, bounded to 300 ms; it never blocks on it.
  Future<void> disconnect({String? snapshotPath}) {
    if (_disposed) return Future.value();
    final fresh = _session.value.fresh;
    final texture = _texture;
    final image = _jpegImage.value;
    _serial++;
    _target = null;
    _attemptSerial = null;
    _cancelRetry();
    _media = null;
    _status = null;
    _view = null;
    _setup = null;
    _end = null;
    _failure = null;
    _identityChanged = false;
    _hasPicture = false;
    _stopHandled = false;
    _reconnectAttempt = 0;
    _protocolStops = 0;
    _controlWanted = false;
    _controlSuppressed = false;
    _cancelControlCooldown();
    _nativePaused = false;
    _stoppedWhileHidden = null;
    _invalidateInput();
    _mediaSerial++;
    _texture = null;
    _jpegImage.value = null;
    _picture.value = null;
    _geometry.value = null;
    _stats.value = DesktopStats.none;
    _publishSession();
    _schedulePolling();
    _cancelJpeg();
    final previous = _closing;
    late final Future<void> closing;
    closing =
        () async {
          await previous;
          if (snapshotPath != null && fresh) {
            await _capture(texture, image, snapshotPath);
          }
          await _disconnectNative();
          try {
            await texture?.dispose();
          } catch (_) {
            // The texture is gone either way.
          }
          _disposeImageLater(image);
        }().whenComplete(() {
          if (identical(_closing, closing)) _closing = null;
        });
    return _closing = closing;
  }

  /// Writes the current picture to [path] (D23): the native snapshot, or the
  /// JPEG picture as PNG. False when there is none or it took over 300 ms.
  Future<bool> capturePicture(String path) =>
      _capture(_texture, _jpegImage.value, path);

  Future<bool> _capture(
    DesktopVideoTexture? texture,
    ui.Image? image,
    String path,
  ) async {
    try {
      if (texture != null) {
        return await texture
            .snapshot(path)
            .timeout(_snapshotBound, onTimeout: () => false);
      }
      if (image != null) {
        return await _writePng(
          image.clone(),
          path,
        ).timeout(_snapshotBound, onTimeout: () => false);
      }
    } catch (_) {
      // Best effort.
    }
    return false;
  }

  static Future<bool> _writePng(ui.Image image, String path) async {
    try {
      final data = await image.toByteData(format: ui.ImageByteFormat.png);
      if (data == null) return false;
      final temporary = File('$path.tmp');
      await temporary.writeAsBytes(data.buffer.asUint8List(), flush: true);
      await temporary.rename(path);
      return true;
    } catch (_) {
      return false;
    } finally {
      image.dispose();
    }
  }

  /// Retries now: a scheduled reconnect, or an ended session's target.
  /// An identity change never retries.
  void retryNow() {
    if (_disposed || _target == null || _identityChanged) return;
    if (_attemptSerial != null) return;
    if (_retryTimer != null) {
      _cancelRetry();
      unawaited(_attempt(_serial));
    } else if (_end != null) {
      _end = null;
      _failure = null;
      _reconnectAttempt = 0;
      _protocolStops = 0;
      unawaited(_attempt(_serial));
    }
  }

  double _refreshHz() {
    try {
      final views = ui.PlatformDispatcher.instance.views;
      final hz = views.isEmpty ? 60.0 : views.first.display.refreshRate;
      return hz.isFinite && hz > 0 ? hz : 60;
    } catch (_) {
      return 60;
    }
  }

  // --------------------------------------------------------------- input

  int _inputEpoch = 0;

  /// The newest move not yet sent, with the input epoch it was sent in.
  (DesktopMove, int)? _pendingMove;

  /// Running for 4 ms after a move went out; moves meanwhile coalesce.
  Timer? _moveGate;

  void _invalidateInput() {
    _inputEpoch++;
    _pendingMove = null;
    _moveGate?.cancel();
    _moveGate = null;
  }

  /// The user's choice between viewing and controlling. Control is asked
  /// for once the picture is fresh; ending it is immediate and local first.
  void control(bool enabled) {
    if (_disposed || _target == null) return;
    _controlWanted = enabled;
    _controlSuppressed = !enabled;
    if (!enabled) _invalidateInput();
    if (_media == null) {
      // Applied once connected.
      _publishSession();
      return;
    }
    _startControlCooldown();
    try {
      _api.control(enabled);
    } catch (_) {
      // Ending control fails closed locally; asking again comes later.
    }
    _pollStatus();
  }

  /// Releases every key and button held on the host.
  void release() {
    if (_disposed || _media == null) return;
    _invalidateInput();
    try {
      _api.release();
    } catch (_) {
      // Losing the session releases them anyway.
    }
  }

  void requestKeyframe() {
    if (_disposed || _media == null || !_visible) return;
    try {
      _api.requestKeyframe();
    } catch (_) {
      // Rate-limited or closed; the next request may pass.
    }
  }

  /// Locks the host (D17), as input: only while controlling.
  void lockRemote() {
    final geometry = _geometry.value;
    if (geometry == null) return;
    send(const DesktopInput.lock(), geometryEpoch: geometry.epoch);
  }

  /// A live change: never reconnects (D12).
  void setQuality(DesktopQuality quality) {
    if (_disposed) return;
    _quality.value = quality;
    final target = _target;
    if (target != null) _target = target.withQuality(quality);
    if (_media != null) {
      try {
        _api.setQuality(quality);
      } catch (_) {
        // The next connect applies it.
      }
    }
    _publishSession();
  }

  bool _accepts(int geometryEpoch) =>
      !_disposed &&
      _session.value.inputLive &&
      _geometry.value?.epoch == geometryEpoch;

  /// False when dropped: not live, or [geometryEpoch] is not the current
  /// one. Moves are coalesced to one per 4 ms; a pending move goes before
  /// any other input.
  bool send(DesktopInput input, {required int geometryEpoch}) {
    if (!_accepts(geometryEpoch)) return false;
    if (input is DesktopMove) {
      if (_moveGate != null) {
        _pendingMove = (input, _inputEpoch);
        return true;
      }
      if (!_deliver([input])) return false;
      _moveGate = Timer(_moveInterval, _flushMove);
      return true;
    }
    return _deliver([...?_takeMove(), input]);
  }

  /// [inputs] go in one atomic, ordered batch, or not at all: on a refusal
  /// everything held is released, so no key stays down on the host (D16).
  bool sendSequence(List<DesktopInput> inputs, {required int geometryEpoch}) {
    assert(inputs.isNotEmpty && inputs.length <= kSequenceMax);
    if (inputs.isEmpty || inputs.length > kSequenceMax) return false;
    if (!_accepts(geometryEpoch)) return false;
    final move = _takeMove();
    if (move != null && inputs.length == kSequenceMax) {
      if (!_deliver(move)) return false;
    }
    final batch = [
      if (move != null && inputs.length < kSequenceMax) ...move,
      ...inputs,
    ];
    if (_deliver(batch)) return true;
    release();
    return false;
  }

  List<DesktopInput>? _takeMove() {
    final pending = _pendingMove;
    _pendingMove = null;
    return pending != null && pending.$2 == _inputEpoch ? [pending.$1] : null;
  }

  /// The gate closes: the newest move waiting behind it goes, and opens the
  /// gate again.
  void _flushMove() {
    _moveGate = null;
    final geometry = _geometry.value;
    final move = _takeMove();
    if (move == null || geometry == null || !_accepts(geometry.epoch)) return;
    if (_deliver(move)) _moveGate = Timer(_moveInterval, _flushMove);
  }

  /// Rust stamps the frame id and sequence; Dart names only the generation.
  bool _deliver(List<DesktopInput> inputs) {
    final geometry = _geometry.value;
    if (geometry == null) return false;
    try {
      _api.input(geometry.generation, [for (final i in inputs) i.toJson()]);
      return true;
    } on DesktopCallException catch (e) {
      if (e.code != 'NOT_FRESH') _interruptInput();
      return false;
    } catch (_) {
      _interruptInput();
      return false;
    }
  }

  /// Control is lost or congested: end it locally; queued input is never
  /// replayed.
  void _interruptInput() {
    _controlSuppressed = true;
    _invalidateInput();
    try {
      _api.control(false);
    } catch (_) {
      // Transport loss already released remote input.
    }
    _publishSession();
  }

  // ------------------------------------------------------------- dispose

  void dispose() {
    if (_disposed) return;
    _cancelHostResume();
    _disposed = true;
    _serial++;
    _target = null;
    _cancelRetry();
    _cancelControlCooldown();
    _hostTimer?.cancel();
    _statusTimer?.cancel();
    _viewTimer?.cancel();
    _statsTimer?.cancel();
    _moveGate?.cancel();
    _framed.remove(this);
    _lifecycle?.dispose();
    unawaited(_jpeg?.cancel());
    _jpeg = null;
    final texture = _texture;
    _texture = null;
    if (texture != null) {
      unawaited(texture.dispose().catchError((Object _) {}));
    }
    _jpegImage.value?.dispose();
    unawaited(_api.disconnect().catchError((Object _) {}));
    unawaited(_api.stopHost(onExit: true).catchError((Object _) {}));
    _hostView.dispose();
    _session.dispose();
    _geometry.dispose();
    _picture.dispose();
    _jpegImage.dispose();
    _stats.dispose();
    _quality.dispose();
  }
}
