import 'dart:async';

import 'package:flutter/foundation.dart';

import 'desktop_store.dart';
import 'remote_computers.dart';
import 'viewer_prefs.dart';

/// A session left in the background by Back ends after this: a paused
/// session still holds the host's only viewer slot, so every other viewer
/// would get `HOST_BUSY` until this one came back.
const kPausedSessionTimeout = Duration(minutes: 10);

/// Whether the viewer is on screen. The app shows the viewer instead of the
/// shell while it is, so there is only ever one. Holding it is what keeps
/// the session resumed (D13); letting go pauses it.
class ViewerHost extends ChangeNotifier {
  ViewerHost(this._desktop);

  final DesktopStore _desktop;
  DesktopDemandHandle? _demand;

  bool get showing => _demand != null;

  void open() {
    if (_demand != null) return;
    _demand = _desktop.demand(DesktopDemand.viewer);
    notifyListeners();
  }

  void close() {
    final demand = _demand;
    if (demand == null) return;
    _demand = null;
    demand.dispose();
    notifyListeners();
  }

  @override
  void dispose() {
    _demand?.dispose();
    _demand = null;
    super.dispose();
  }
}

/// What the add flow hands over: a recognised invitation and what to do
/// with it.
@immutable
class AddComputerResult {
  const AddComputerResult({
    required this.invitation,
    required this.info,
    required this.name,
    this.remember = true,
    this.relayOnly = false,
    this.replaceId,
    this.existing,
  });

  /// The pasted or scanned invitation: a bearer secret, never shown or
  /// logged.
  final String invitation;
  final DesktopInvitationInfo info;
  final String name;
  final bool remember, relayOnly;

  /// The remembered computer whose saved invitation this one replaces.
  final String? replaceId;

  /// Already remembered with this identity: it is opened instead.
  final RemoteComputer? existing;

  @override
  String toString() =>
      'AddComputerResult($name, ${info.service}, remember: $remember, '
      'replaces: $replaceId, existing: ${existing?.id})';
}

/// A remember waiting for its session's first picture.
class _PendingRemember {
  const _PendingRemember({
    required this.id,
    required this.name,
    required this.service,
    required this.fingerprint,
    required this.relayOnly,
    this.replaceId,
  });
  final String id, name, service, fingerprint;
  final bool relayOnly;
  final String? replaceId;
}

/// Viewing other computers: which one is open, opening, leaving and ending
/// it, and remembering a new one once it has shown its first picture. It
/// holds no `BuildContext`; confirmations are the pages' (U26).
class RemoteDesktop {
  RemoteDesktop({
    required this.desktop,
    required this.computers,
    required this.viewer,
    required this.prefs,
    required String Function() relayAddress,
    required int Function() credentialKeyId,
    DateTime Function()? now,
  }) : _relayAddress = relayAddress,
       _credentialKeyId = credentialKeyId,
       _now = now ?? DateTime.now {
    desktop.session.addListener(_onSession);
    viewer.addListener(_onViewer);
  }

  final DesktopStore desktop;
  final RemoteComputerStore computers;
  final ViewerHost viewer;
  final ViewerPrefs prefs;
  final String Function() _relayAddress;

  /// The credential a computer is remembered under (D29).
  final int Function() _credentialKeyId;
  final DateTime Function() _now;

  _PendingRemember? _pending;

  /// The target whose first picture was already handled.
  String? _liveKey;
  Timer? _pausedTimer;

  static String? _key(DesktopTarget? target) =>
      target?.computerId ?? target?.invitation;

  static bool _open(DesktopSessionView session) =>
      session.target != null &&
      session.phase != DesktopPhase.idle &&
      session.phase != DesktopPhase.ended;

  /// The title the viewer shows: the remembered name, else the target's.
  String get title {
    final target = desktop.session.value.target;
    if (target == null) return '';
    final id = target.computerId;
    return (id == null ? null : computers.byId(id))?.name ?? target.name;
  }

  /// Whether opening [c] would end another open session (a switch to
  /// confirm).
  bool switchesFrom(RemoteComputer c) {
    final session = desktop.session.value;
    return session.target?.computerId != c.id &&
        _open(session) &&
        session.phase != DesktopPhase.identityChanged;
  }

  /// Shows [c]: its open session if it has one, else a new one, ending
  /// whatever else was open.
  Future<void> open(RemoteComputer c) async {
    _cancelPausedTimer();
    final session = desktop.session.value;
    if (session.target?.computerId == c.id && _open(session)) {
      viewer.open();
      return;
    }
    // On Android a connect by id needs the vault unlocked first.
    await computers.load();
    _pending = null;
    unawaited(_endSession());
    unawaited(
      desktop.connect(
        DesktopTarget(
          name: c.name,
          computerId: c.id,
          relayOnly: c.relayOnly,
          quality: c.quality,
        ),
      ),
    );
    viewer.open();
  }

  /// Connects what the add flow recognised. A computer to remember is
  /// remembered only after its first picture, never before (D20).
  Future<void> connectNew(AddComputerResult result) async {
    if (result.existing case final existing?) return open(existing);
    _cancelPausedTimer();
    final replaceId = result.replaceId;
    final id = replaceId ?? computers.newId();
    _pending = result.remember
        ? _PendingRemember(
            id: id,
            name: result.name,
            service: result.info.service,
            fingerprint: result.info.fingerprint,
            relayOnly: result.relayOnly,
            replaceId: replaceId,
          )
        : null;
    unawaited(_endSession());
    unawaited(
      desktop.connect(
        DesktopTarget(
          invitation: result.invitation,
          name: result.name,
          computerId: id,
          relayOnly: result.relayOnly,
          quality:
              (replaceId == null ? null : computers.byId(replaceId))?.quality ??
              DesktopQuality.balanced,
        ),
      ),
    );
    viewer.open();
  }

  /// Leaves the picture and keeps the session, paused (D13): back to the
  /// list. A session left like this ends after [kPausedSessionTimeout].
  void back() {
    final session = desktop.session.value;
    final id = _rememberedId(session);
    final file = id == null ? null : computers.thumbnailFile(id);
    if (id != null &&
        file != null &&
        session.fresh &&
        session.phase != DesktopPhase.identityChanged) {
      unawaited(
        desktop.capturePicture(file.path).then((written) {
          if (written) computers.thumbnailWritten(id);
        }),
      );
    }
    viewer.close();
    if (session.target != null) {
      _cancelPausedTimer();
      _pausedTimer = Timer(kPausedSessionTimeout, () {
        _pausedTimer = null;
        unawaited(disconnect());
      });
    }
  }

  /// Ends the session, with its last picture (D23), and closes the viewer.
  Future<void> disconnect() async {
    _cancelPausedTimer();
    _pending = null;
    final ending = _endSession();
    viewer.close();
    await ending;
  }

  /// Connects the ended session's computer again.
  Future<void> reconnect() async {
    _cancelPausedTimer();
    final target = desktop.session.value.target;
    if (target == null) return;
    if (target.invitation == null) await computers.load();
    await desktop.connect(target);
  }

  /// A live change (D12), kept for a remembered computer.
  Future<void> setQuality(DesktopQuality quality) async {
    desktop.setQuality(quality);
    final id = _rememberedId(desktop.session.value);
    if (id != null) await computers.setQuality(id, quality);
  }

  String? _rememberedId(DesktopSessionView session) {
    final id = session.target?.computerId;
    return id != null && computers.byId(id) != null ? id : null;
  }

  /// Disconnects the store at once, before any await, so the picture is
  /// still the one being ended when the snapshot is taken.
  Future<void> _endSession() async {
    final session = desktop.session.value;
    if (session.target == null) return;
    final id = _rememberedId(session);
    final file = id == null || session.phase == DesktopPhase.identityChanged
        ? null
        : computers.thumbnailFile(id);
    if (id != null && _liveKey == id) {
      unawaited(
        computers.recordSession(
          id,
          at: _now(),
          path: session.path,
          controlAllowed: session.controlAllowed,
        ),
      );
    }
    _liveKey = null;
    final closing = desktop.disconnect(snapshotPath: file?.path);
    await closing;
    if (id != null && file != null && session.fresh) {
      computers.thumbnailWritten(id);
    }
  }

  void _onSession() {
    final session = desktop.session.value;
    final key = _key(session.target);
    if (key == null) return;
    if (_liveKey == key) return;
    if (session.phase == DesktopPhase.live && session.hasPicture) {
      _liveKey = key;
      unawaited(_firstPicture(session));
    } else if ((session.phase == DesktopPhase.identityChanged ||
            session.phase == DesktopPhase.ended) &&
        _pending?.id == key) {
      // Not proven to be the computer it claims: nothing is remembered.
      _pending = null;
    }
  }

  Future<void> _firstPicture(DesktopSessionView session) async {
    final target = session.target;
    final id = target?.computerId;
    if (target == null || id == null) return;
    final pending = _pending;
    if (pending != null && pending.id == id) {
      _pending = null;
      await computers.rememberSession(
        id: id,
        name: pending.name,
        service: pending.service,
        fingerprint: pending.fingerprint,
        relay: _relayAddress(),
        namespace: _credentialKeyId(),
        relayOnly: pending.relayOnly,
        quality: target.quality,
        replaceId: pending.replaceId,
      );
    }
    if (computers.byId(id) != null) {
      await computers.recordSession(
        id,
        at: _now(),
        path: session.path,
        controlAllowed: session.controlAllowed,
      );
    }
  }

  void _onViewer() {
    if (viewer.showing) _cancelPausedTimer();
  }

  void _cancelPausedTimer() {
    _pausedTimer?.cancel();
    _pausedTimer = null;
  }

  void dispose() {
    _cancelPausedTimer();
    desktop.session.removeListener(_onSession);
    viewer.removeListener(_onViewer);
  }
}
