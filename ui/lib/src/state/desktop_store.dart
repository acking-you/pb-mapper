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

/// How long a run of failed attempts goes on before the session ends as
/// [DesktopEndReason.unreachable]: a host that is off or not sharing, a
/// hand-off (`restarting`) that never completes, a host that stays busy.
/// What comes back by itself does so well inside it: a host that lost the
/// relay tries to register again at least every 2 s, and a refused
/// registration after at most 80 s
/// (`PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX`), so with one 15 s backoff
/// step and an attempt's setup budget it is found again in under two minutes.
const _retryWindow = Duration(minutes: 2);
const _protocolRetryLimit = 3;
const _noVideoReconnectMs = 5000;
const _statusVisible = Duration(milliseconds: 500);
const _statusHidden = Duration(seconds: 1);
const _hostWithPage = Duration(seconds: 1);
const _hostWhileSharing = Duration(seconds: 2);
const _viewTick = Duration(milliseconds: 16);
const _moveInterval = Duration(milliseconds: 4);
const _snapshotBound = Duration(milliseconds: 300);
const _controlRetry = Duration(seconds: 1);
const _statsInterval = Duration(seconds: 1);

/// The host's 1 s `STOP_WRITE_TIMEOUT`, plus a worker isolate's spawn.
const _shutdownBound = Duration(milliseconds: 1500);

/// How long a connect waits for the decoder probe (I4), which enumerates
/// in driver code and may never answer.
const _decoderProbeBound = Duration(seconds: 2);

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
///
/// A widget that rebuilds around a fresh `Listenable.merge` takes its
/// listener out and puts it back in the same frame; that is not a loss, so
/// the last listener counts as gone only if none is back by the next
/// microtask.
class _SampledNotifier<T> extends ValueNotifier<T> {
  _SampledNotifier(super.value, this._listenersChanged);
  final void Function() _listenersChanged;
  bool _leaving = false;

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
    if (hasListeners || _leaving) return;
    _leaving = true;
    scheduleMicrotask(() {
      _leaving = false;
      if (!hasListeners) _listenersChanged();
    });
  }
}

/// The remote cursor as this viewer knows it (D12): its own, which strokes,
/// taps and the mouse move, and which the cursor layer draws. The store
/// owns it for the session, not the viewer on screen, so a viewer opened
/// again after Back resumes it where the last one left it.
///
/// The host's reports never take it back on a timer. One is followed only
/// where it tells this viewer something its own cursor cannot know: the
/// first of a connection or a display, every one while viewing only, and
/// else a new report more than a pixel from this cursor — the remote user,
/// a program or a clamp moved it. The host never reports a position it
/// injected for this viewer, so a report already seen is old news, however
/// late a viewer comes to it.
final class DesktopCursor extends ChangeNotifier {
  Offset _position = const Offset(0.5, 0.5);

  /// The arrow is drawn: the host showed its cursor, or this viewer moved
  /// it since the host last said it is hidden.
  bool _shown = false;

  /// The last host report seen, and the geometry epoch of the last one
  /// that was not hidden.
  Offset? _report;
  int? _reportEpoch;

  /// Where taps click and strokes start, normalised to the display.
  Offset get position => _position;

  /// Where the arrow is drawn; null while the host hides its cursor and
  /// this viewer has not moved it since.
  Offset? get shown => _shown ? _position : null;

  /// A local move: the arrow shows here.
  void moveTo(Offset normalized) {
    if (normalized == _position && _shown) return;
    _position = normalized;
    _shown = true;
    notifyListeners();
  }

  /// The host's cursor as the store last read it, null while hidden or
  /// unknown, read against [g]. True when what is drawn changed.
  bool _onReport(
    Offset? report,
    DesktopGeometry? g, {
    required bool controlling,
  }) {
    if (report == _report) return false;
    _report = report;
    if (report == null) {
      // Hidden, off the shared display, or not reported yet.
      if (!_shown) return false;
      _shown = false;
      notifyListeners();
      return true;
    }
    final first = g == null || g.epoch != _reportEpoch;
    _reportEpoch = g?.epoch;
    final follow = first || !controlling || _apart(report, g);
    if (!follow && _shown) return false;
    if (follow) _position = report;
    _shown = true;
    notifyListeners();
    return true;
  }

  /// More than a display pixel from the pixel this cursor is sent as: the
  /// host reports whole pixels, and this cursor keeps the fraction.
  bool _apart(Offset report, DesktopGeometry? g) {
    if (g == null) return true;
    bool far(double reported, double local, int size) =>
        ((reported * size).round() - (local * size).floor()).abs() > 1;
    return far(report.dx, _position.dx, g.displayWidth) ||
        far(report.dy, _position.dy, g.displayHeight);
  }

  /// Another computer: nothing of the last one's cursor carries over.
  void _reset() {
    _position = const Offset(0.5, 0.5);
    _report = null;
    _reportEpoch = null;
    if (!_shown) return;
    _shown = false;
    notifyListeners();
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
       _onRestartReady = onRestartReady {
    _geometry.addListener(_publishCursor);
  }

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
  late final _decoder = _SampledNotifier<DesktopDecoderStats?>(
    null,
    _schedulePolling,
  );
  final _quality = ValueNotifier(DesktopQuality.balanced);
  final _cursor = DesktopCursor();
  final _decoders = ValueNotifier<DesktopDecoders?>(null);

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

  /// The native decoder's own figures, read once a second only while
  /// something shows them (the quality panel); null without a native
  /// decoder, and until its second read.
  ValueListenable<DesktopDecoderStats?> get decoderStats => _decoder;
  ValueListenable<DesktopQuality> get quality => _quality;

  /// The remote cursor, the session's: what the viewer moves, and the
  /// host's reports where they say something new. The view leaf reports
  /// them, read every rendered frame and every 16 ms (D11).
  DesktopCursor get cursor => _cursor;

  /// What this device decodes natively, probed once, at the first connect
  /// (I4); null until the probe answers. H.265 leaves it for the rest of the
  /// run once a picture failed to decode as H.265 (D5).
  ValueListenable<DesktopDecoders?> get decoders => _decoders;

  /// The highest frame rate this display shows: what the probe reported,
  /// else its current refresh rate.
  double get displayMaxHz => _decoders.value?.displayMaxHz ?? _refreshHz();

  // ---------------------------------------------------------------- demand

  int _pageDemand = 0, _viewerDemand = 0;
  AppLifecycleListener? _lifecycle;
  AppLifecycleState? _lifecycleState;
  bool _wasVisible = false;

  /// The stores that read the view leaf every rendered frame. Persistent
  /// frame callbacks cannot be removed, so one serves them all.
  static final _framed = <DesktopStore>{};
  static bool _frameHooked = false;

  /// [DesktopDemand.page] polls the host every second where this device can
  /// host; [DesktopDemand.viewer] is a viewer on screen: the session resumes
  /// while it is held and the app is visible, and pauses otherwise (D13).
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

  Timer? _hostTimer, _statusTimer, _viewTimer, _statsTimer, _decoderTimer;
  Duration? _hostEvery, _statusEvery;

  bool get _sessionActive =>
      _target != null && _end == null && !_identityChanged;
  bool get _viewPolling => _visible && _sessionActive;

  void _schedulePolling() {
    final hostEvery = _disposed || _restarting || _cannotHost
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
    final decoderOn = !_disposed && _sessionActive && _decoder.listened;
    if (decoderOn != (_decoderTimer != null)) {
      _decoderTimer?.cancel();
      _decoderRead = null;
      if (decoderOn) {
        _decoderTimer = Timer.periodic(
          _statsInterval,
          (_) => unawaited(_readDecoder()),
        );
        unawaited(_readDecoder());
      } else {
        _decoderTimer = null;
        if (!_disposed) _decoder.value = null;
        // The decoder stops timing until the next look.
        unawaited(
          _texture?.stats(timings: false).then((_) {}, onError: (Object _) {}),
        );
      }
    }
  }

  // ================================================================= host

  DesktopCapabilities? _capabilities;
  DesktopTrust? _trust;

  /// Why the remembered grant could not be read. It stays until a read
  /// succeeds, which for a grant Windows cannot unlock only Revoke brings.
  String? _trustError;
  DesktopHostSnapshot? _host;
  bool _hostStatusRead = false, _hostBusy = false;
  bool _enabling = false, _restarting = false, _startupRestored = false;
  String? _hostError;
  int _hostSerial = 0, _hostResumeAttempt = 0;
  int _hostReads = 0, _hostApplied = 0;
  Timer? _hostResumeTimer;
  Future<void>? _hostLoading;

  bool _hostCurrent(int serial) => !_disposed && serial == _hostSerial;

  /// The page already says this device cannot host. The capability is
  /// fixed at build time, so nothing about the host can change.
  bool get _cannotHost => _hostView.value.loaded && !_hostView.value.supported;

  Future<void> _onPageDemand() async {
    if (_cannotHost) return;
    // An unreadable grant is read again on each visit, never on a timer.
    if (_hostView.value.loaded && _trustError == null) {
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
      await _readTrust(_hostSerial);
      await _discoverBackground();
      await _readHost();
    } catch (e) {
      if (!_disposed) _hostError = e.toString();
      _publishHost();
    } finally {
      _hostLoading = null;
    }
  }();

  /// Reads the remembered grant; null when it cannot be read as one.
  /// Only `PROTOCOL` says that (Windows cannot unlock the grant, or it is
  /// invalid), and it is kept, not thrown: reading again does not mend it,
  /// and the card must still load to offer the Revoke its message asks
  /// for. Any other failure is thrown, for the caller to retry.
  Future<DesktopTrust?> _readTrust(int serial) async {
    DesktopTrust? trust;
    String? error;
    try {
      trust = await _api.trustStatus();
    } on DesktopCallException catch (e) {
      if (e.code != 'PROTOCOL') rethrow;
      error = e.toString();
    }
    if (_hostCurrent(serial)) {
      _trust = trust;
      _trustError = error;
    }
    return trust;
  }

  /// `host_status` reads the cached background state; this asks the
  /// service itself, which only the startup and explicit actions do.
  Future<void> _discoverBackground() async {
    final background = await _api.backgroundStatus();
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
      // A lifecycle call holds the host, so it is unknown, not gone: keep
      // the one this store has until a read that is not busy.
      _host = snapshot.busy
          ? DesktopHostSnapshot(
              host: _host?.host,
              background: snapshot.background,
            )
          : snapshot;
      _hostStatusRead = true;
    } catch (_) {
      // A failed poll keeps the last state; actions report their own errors.
    }
    _publishHost();
  }

  Future<void> _readTrustAndHost(int serial) async {
    await _readTrust(serial);
    if (!_hostCurrent(serial)) return;
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
    final trustError = _trustError;
    _hostView.value = DesktopHostView(
      loaded:
          capabilities != null &&
          (trust != null || trustError != null) &&
          _hostStatusRead,
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
      trustUnreadable: trustError != null,
      error: _hostError ?? trustError,
      displays: capabilities?.displays ?? const [],
      displayId:
          host?.display?.id ??
          backgroundHost?.display?.id ??
          (configuredBackground ? background!.displayId : null) ??
          saved?.displayId,
      helper: _helperState(capabilities?.helper),
      viewer: host?.viewer ?? backgroundHost?.viewer,
      // The running host's relay, else the background service's, when the
      // settings name another one. A remembered grant for another relay is
      // not configured at all.
      otherRelay: host != null
          ? (host.matchesRelay ? null : host.server)
          : configuredBackground && !background!.matchesRelay
          ? background.server
          : null,
    );
    _schedulePolling();
  }

  static DesktopHelperState _helperState(DesktopHelper? helper) {
    if (helper == null || !helper.installed) return DesktopHelperState.none;
    // Another build still works, but hosts in the background with its own
    // code: Update installs this one.
    if (helper.updateRequired || helper.updateAvailable) {
      return DesktopHelperState.outdated;
    }
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
      // service IPC. A grant that cannot be read as one restores nothing
      // and is not retried in the background: the card offers Revoke, and
      // each page visit reads it again. A failed read is retried.
      final trust = await _readTrust(serial) ?? const DesktopTrust();
      if (!_hostCurrent(serial)) return;
      // A computer that cannot share (Windows without Media Foundation)
      // restores nothing and retries nothing: the grant stays for a start
      // where it can, and the Remote page says why. Local, no service IPC.
      _capabilities = await _api.capabilities();
      if (!_hostCurrent(serial)) return;
      if (_capabilities?.host != true) {
        _publishHost();
        return;
      }
      if (trust.saved?.background == true &&
          trust.saved?.autoStart == true &&
          trust.matchesRelay) {
        await _resumePendingHost(serial);
        return;
      }
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
      final trust = await _readTrust(serial) ?? const DesktopTrust();
      if (!_hostCurrent(serial)) return;
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
        final result = await _setUp(
          serial,
          () => _api.enableHost(name, displayId, allowControl: allowControl),
        );
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

  /// [call], which may set up the helper. A failed IPC reply can leave
  /// durable setup waiting for the helper: that intent resumes through
  /// HostRestore, which never installs or asks for UAC. A cancelled
  /// installation has already retired it.
  Future<DesktopHostEnable> _setUp(
    int serial,
    Future<DesktopHostEnable> Function() call,
  ) async {
    try {
      return await call();
    } catch (_) {
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
  }

  /// Starts the remembered or configured host again. The background
  /// service's is configured again from the settings; on another relay that
  /// gives it a new invitation, so the card asks first
  /// ([DesktopHostView.otherRelay], [shareOnCurrentRelay]), and Rust refuses
  /// a move it was not asked for.
  Future<void> resumeHost() => _resumeHost(moveRelay: false);

  Future<void> _resumeHost({required bool moveRelay}) async {
    await _hostAction(enabling: true, () async {
      _cancelHostResume();
      final serial = _hostSerial;
      if (_host?.background?.configured == true) {
        // A helper that is not ready, or another build, is set up first, as
        // turning remote control on does, and may restart the app into it.
        final DesktopHostEnable result;
        try {
          result = await _setUp(
            serial,
            () => _api.resumeBackground(moveRelay: moveRelay),
          );
        } on DesktopCallException catch (e) {
          if (e.code != 'RELAY_CHANGED') rethrow;
          // The settings name another relay than the card knew: read again,
          // and it says so and asks.
          if (_hostCurrent(serial)) await _readHost();
          return;
        }
        if (!_hostCurrent(serial)) return;
        if (result.restart) {
          _restart();
          return;
        }
        _host = DesktopHostSnapshot(
          host: _host?.host,
          background: result.background,
        );
      } else {
        await _api.restoreHost();
      }
      if (!_hostCurrent(serial)) return;
      await _readTrustAndHost(serial);
    });
  }

  /// Shares on the relay the settings name now what is shared, or paused,
  /// on another one ([DesktopHostView.otherRelay]), with a new invitation:
  /// the background service's host resumes there, and a running local host
  /// starts again there with its settings.
  Future<void> shareOnCurrentRelay() async {
    final local = _host?.host;
    if (local == null) return _resumeHost(moveRelay: true);
    final saved = _trust?.saved;
    await _hostAction(enabling: true, () async {
      _cancelHostResume();
      final serial = _hostSerial;
      final display =
          local.display?.id ??
          saved?.displayId ??
          _capabilities?.displays.firstOrNull?.id;
      if (display == null) throw StateError('No display to share');
      await _api.stopHost();
      if (!_hostCurrent(serial)) return;
      await _api.startHost(
        service: local.service ?? saved?.service ?? 'desktop:computer',
        displayId: display,
        allowControl: local.controlAllowed,
        relayOnly: saved?.relayOnly ?? false,
        privileged: saved?.privileged ?? false,
        remember: true,
        autoStart: saved?.autoStart ?? true,
      );
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

  /// The latest attempt offered H.265.
  bool _offeredH265 = false;

  /// This computer answered an offer of H.265 with H.264 (an older build,
  /// no encoder, or one that failed): its sessions are H.264 from then on.
  bool _h265Declined = false;
  Future<DesktopDecoders>? _probing;
  int _reconnectAttempt = 0, _protocolStops = 0;
  Timer? _retryTimer;
  DateTime? _retryAt;

  /// When the current run of failed attempts began; null between runs.
  DateTime? _failingSince;

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
    _resetRetries();
    _end = null;
    _failure = null;
    _identityChanged = false;
    _stoppedWhileHidden = null;
    _controlWanted = true;
    _cancelControlCooldown();
    _quality.value = target.quality;
    if (!sameComputer) {
      // Another computer's last picture, cursor and codec answer must not
      // carry over to this one.
      _hasPicture = false;
      _releaseMedia();
      _cursor._reset();
      _h265Declined = false;
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
    if (_disposed || _target == null || serial != _serial) return;
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
      final decoders = _decoders.value ?? await _probeDecoders();
      // Read after the waits: a change meanwhile applies to this attempt.
      final target = _target;
      if (_disposed || target == null || serial != _serial) return;
      final h265 = _offeredH265 = _offersH265(decoders, target);
      // Never a size the decoder of the codec asked for cannot take.
      final resolution = h265
          ? target.resolution
          : decoders.forH264(target.resolution);
      final session = await _api.connect(
        target.invitation,
        computerId: target.computerId,
        relayOnly: target.relayOnly,
        quality: _quality.value,
        refreshHz: _refreshHz(),
        h265: h265,
        resolution: resolution,
        maxFps: target.maxFps,
      );
      if (_disposed || serial != _serial) return;
      if (h265 && session.codec == 'h264') {
        _h265Declined = true;
        if (decoders.forH264(resolution) != resolution) {
          // Only H.265 decodes this size here: again at once, at the
          // largest H.264 does. The computer keeps the choice.
          _reconnectNow();
          return;
        }
      }
      _attemptSerial = null;
      _media = session;
      _stopHandled = false;
      _nativePaused = false;
      _controlSuppressed = false;
      _cancelControlCooldown();
      _setup = DesktopSetupStep.firstPicture;
      // A new Rust viewer: new sequences, so nothing older may be sent, and
      // no cursor is known until it reports one.
      _invalidateInput();
      _view = null;
      _publishCursor();
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
    if (failure.code == 'DECODER' && _fallBackFromH265()) return;
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
    final now = _now();
    if (now.difference(_failingSince ??= now) >= _retryWindow) {
      // Nothing came back by itself: say why, and leave the next try to
      // the user.
      _endSession(DesktopEndReason.unreachable, failure);
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
    _retryAt = now.add(delay);
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

  /// A new run: on a connect, a live picture, a disconnect, or a retry after
  /// the end.
  void _resetRetries() {
    _reconnectAttempt = 0;
    _protocolStops = 0;
    _failingSince = null;
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
    } else if (code != 'decoder_failed' || !_fallBackFromH265()) {
      _endSession(_endReason(code), failure);
    }
  }

  /// D5: this device failed to decode the session's H.265. H.265 is not
  /// offered again this run, and the session connects again at once, on
  /// H.264; only a failure there reaches the user. False for any other
  /// codec.
  bool _fallBackFromH265() {
    if (_media?.codec != 'h265') return false;
    _decoders.value = (_decoders.value ?? DesktopDecoders.h264Only)
        .withoutH265();
    _reconnectNow();
    return true;
  }

  /// Connects the session's computer again at once, keeping its picture,
  /// the control choice and the preset: for what only a new session
  /// carries (resolution, frame rate, codec), or a codec that failed. A run
  /// of failures starts over. An ended session takes the change at its
  /// next connect.
  void _reconnectNow() {
    if (_disposed || !_sessionActive) return;
    _cancelRetry();
    _resetRetries();
    _failure = null;
    _stoppedWhileHidden = null;
    unawaited(_attempt(++_serial));
  }

  /// The decoders the first connect probes; later ones reuse the answer
  /// ([decoders]) without waiting. Until a probe that has not answered
  /// within [_decoderProbeBound] (a wedged driver) does, attempts are H.264
  /// only and none waits again; the platform is asked once, and its answer,
  /// however late, is kept.
  Future<DesktopDecoders> _probeDecoders() => _probing ??= _api
      .decoders()
      .catchError((Object _) => DesktopDecoders.h264Only)
      .then((probed) {
        if (_disposed) return probed;
        return _decoders.value ??= probed;
      })
      .timeout(_decoderProbeBound, onTimeout: () => DesktopDecoders.h264Only);

  /// D6, and what this computer answered: H.265 is offered when the device
  /// would offer it and the computer has not declined it.
  bool _offersH265(DesktopDecoders? decoders, DesktopTarget target) =>
      !_h265Declined && (decoders?.offersH265(target) ?? false);

  /// D7: a session at [r] would decode here, judged by the codec it would
  /// carry — H.265 where it would be offered, else H.264. Before the probe
  /// has answered, every size.
  bool decodes(DesktopResolution r) {
    final decoders = _decoders.value;
    if (decoders == null) return true;
    return decoders.decodes(
      r,
      _target?.codec ?? DesktopCodecPreference.auto,
      h265Declined: _h265Declined,
    );
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
          _resetRetries();
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
    final session = _session.value;
    if (session.inputLive != _wasLive) {
      _wasLive = session.inputLive;
      final geometry = _geometry.value;
      if (_wasLive) {
        _resendKept();
      } else if (session.controlling && geometry != null) {
        // Only the picture went stale: the newest move waits for the next
        // fresh one.
        final move = _takeMove()?.single;
        _invalidateInput();
        if (move is DesktopMove) _keep(move, geometry.epoch);
      } else {
        _invalidateInput();
      }
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

  /// The leaf runs every rendered frame, and from a timer at the rate the
  /// host samples its cursor. A report that moves [cursor] repaints it, and
  /// asks for the frame that draws it. A change of generation, state or
  /// flags re-reads the status, which publishes it.
  void _acceptView(DesktopView? view, {required bool inFrame}) {
    final previous = _view;
    _view = view;
    // After paint a repaint asks for no frame: ask for the one that draws
    // the new cursor.
    if (_publishCursor() && inFrame) {
      SchedulerBinding.instance.scheduleFrame();
    }
    if (view == null) return;
    if (_keptMove != null &&
        view.fresh &&
        view.presentedFrameId != _keptAtFrame) {
      _resendKept();
    }
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
  }

  /// Normalises the last leaf read against [geometry], the one place the
  /// host's cursor meets the display it moves on, and reports it to
  /// [cursor]: null while hidden, unknown, of another generation than
  /// [geometry], or not yet reported by this connection. True when what
  /// [cursor] draws changed.
  bool _publishCursor() {
    final view = _view, g = _geometry.value;
    final report =
        view == null ||
            g == null ||
            !view.cursorVisible ||
            view.generation != g.generation ||
            g.displayWidth <= 0 ||
            g.displayHeight <= 0
        ? null
        : Offset(
            (view.cursorX / g.displayWidth).clamp(0.0, 1.0),
            (view.cursorY / g.displayHeight).clamp(0.0, 1.0),
          );
    return _cursor._onReport(
      report,
      g,
      controlling: _session.value.controlling,
    );
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
    // Nothing retries while hidden: a run's window starts again when the
    // viewer shows.
    _failingSince = null;
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

  /// The decoder's last read, which the next one is measured against.
  ({DesktopVideoTexture texture, Map<String, Object?> stats, DateTime at})?
  _decoderRead;

  Future<void> _readDecoder() async {
    final texture = _texture;
    // A read belongs to the timer that made it: one that answers after its
    // timer stopped, even if another has started since, is not measured
    // against the new timer's reads.
    final timer = _decoderTimer;
    if (texture == null || timer == null) return;
    final Map<String, Object?> stats;
    try {
      stats = await texture.stats();
    } catch (_) {
      return;
    }
    if (_disposed || !identical(_decoderTimer, timer)) return;
    final at = _now();
    final last = _decoderRead;
    _decoderRead = (texture: texture, stats: stats, at: at);
    if (last == null || !identical(last.texture, texture)) return;
    _decoder.value = DesktopDecoderStats.between(
      last.stats,
      stats,
      at.difference(last.at),
    );
  }

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
        // Between sessions, the last one's.
        codec: _media?.codec ?? _picture.value?.codec ?? 'h264',
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
    _resetRetries();
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
      _resetRetries();
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

  /// The newest move refused because the picture was not fresh, or held
  /// back as it went stale, with the geometry epoch it belongs to. It goes
  /// with the next fresh picture, so the last position always reaches the
  /// host (D12); whatever else ends input drops it.
  (DesktopMove, int)? _keptMove;

  /// The presented frame the kept move was refused at: only a newer fresh
  /// one sends it again.
  int _keptAtFrame = 0;

  /// Running for 4 ms after a move went out; moves meanwhile coalesce.
  Timer? _moveGate;

  void _invalidateInput() {
    _inputEpoch++;
    _pendingMove = null;
    _keptMove = null;
    _moveGate?.cancel();
    _moveGate = null;
  }

  void _keep(DesktopMove move, int geometryEpoch) {
    _keptMove = (move, geometryEpoch);
    _keptAtFrame = _view?.presentedFrameId ?? 0;
  }

  /// The kept move goes, now that the picture is fresh again.
  void _resendKept() {
    final kept = _keptMove;
    // An open gate sends it with whatever waits behind it.
    if (kept == null || _moveGate != null || !_accepts(kept.$2)) return;
    _keptMove = null;
    if (_deliver([kept.$1])) _moveGate = Timer(_moveInterval, _flushMove);
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
    if (target != null) _target = target.copyWith(quality: quality);
    if (_media != null) {
      try {
        _api.setQuality(quality);
      } catch (_) {
        // The next connect applies it.
      }
    }
    _publishSession();
  }

  /// The encoded size's bound (D7), fixed for a session: a change connects
  /// again at once.
  void setResolution(DesktopResolution resolution) =>
      _setLimits(_target?.copyWith(resolution: resolution));

  /// The frame rate's ceiling (D8), 0 for Auto, fixed for a session: a
  /// change connects again at once.
  void setMaxFps(int fps) => _setLimits(_target?.copyWith(maxFps: fps));

  void _setLimits(DesktopTarget? next) {
    if (_disposed || next == null || next == _target) return;
    _target = next;
    _publishSession();
    _reconnectNow();
  }

  /// The device's codec preference (D6), which a session takes at connect.
  /// It connects again at once only where the session would differ: H.265
  /// is offered now, or the session decodes H.265 and is not to.
  void setCodec(DesktopCodecPreference codec) {
    final target = _target;
    if (_disposed || target == null || target.codec == codec) return;
    final next = _target = target.copyWith(codec: codec);
    _publishSession();
    final offer = _offersH265(_decoders.value, next);
    if (offer == _offeredH265) return;
    // Offered and not taken (the host cannot encode it): nothing changes.
    if (!offer && _media?.codec == 'h264') return;
    _reconnectNow();
  }

  bool _accepts(int geometryEpoch) =>
      !_disposed &&
      _session.value.inputLive &&
      _geometry.value?.epoch == geometryEpoch;

  /// False when dropped: not live, or [geometryEpoch] is not the current
  /// one. Moves are coalesced to one per 4 ms; a pending or kept move goes
  /// before any other input.
  bool send(DesktopInput input, {required int geometryEpoch}) {
    if (!_accepts(geometryEpoch)) return false;
    if (input is DesktopMove) {
      if (_moveGate != null) {
        _pendingMove = (input, _inputEpoch);
        return true;
      }
      // Newer than any kept move.
      _keptMove = null;
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

  /// The newest move not sent yet: the pending one, else the kept one.
  List<DesktopInput>? _takeMove() {
    final pending = _pendingMove, kept = _keptMove;
    _pendingMove = null;
    _keptMove = null;
    if (pending != null && pending.$2 == _inputEpoch) return [pending.$1];
    return kept != null && kept.$2 == _geometry.value?.epoch ? [kept.$1] : null;
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
  /// `NOT_FRESH` drops the batch, but keeps its move for the next fresh
  /// picture; any other refusal ends control.
  bool _deliver(List<DesktopInput> inputs) {
    final geometry = _geometry.value;
    if (geometry == null) return false;
    try {
      _api.input(geometry.generation, [for (final i in inputs) i.toJson()]);
      return true;
    } on DesktopCallException catch (e) {
      if (e.code != 'NOT_FRESH') {
        _interruptInput();
      } else if (inputs.whereType<DesktopMove>().lastOrNull case final move?) {
        _keep(move, geometry.epoch);
      }
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

  /// Quitting: ends this app's sessions so each far side hears why — a
  /// viewer of the in-app host is told `host_stopped`, and the host this app
  /// views sees a clean close. The service's background host is not this
  /// app's to stop. Bounded, so a wedged native call cannot hold the exit.
  Future<void> shutdown() async {
    // The session ends here, as on dispose: no status read may take the
    // closing viewer for a lost one, and no retry may connect again.
    _serial++;
    _target = null;
    _cancelRetry();
    _schedulePolling();
    try {
      await Future.wait([
        _disconnectNative(),
        _api.stopHost(onExit: true).catchError((Object _) {}),
      ]).timeout(_shutdownBound);
    } on TimeoutException {
      // Exit anyway: the far side then sees the transport close.
    }
  }

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
    _decoderTimer?.cancel();
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
    unawaited(shutdown());
    _hostView.dispose();
    _session.dispose();
    _geometry.dispose();
    _picture.dispose();
    _jpegImage.dispose();
    _stats.dispose();
    _decoder.dispose();
    _quality.dispose();
    _cursor.dispose();
    _decoders.dispose();
  }
}
