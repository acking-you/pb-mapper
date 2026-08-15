import 'dart:async';

/// Which list a change invalidated.
enum StateChangeKind {
  services,
  clients,
  server,
  config,
  /// A kind this build does not know. Newer Rust, older Dart — treated as
  /// "something changed", which is the safe reading for an invalidation hint.
  unknown;

  static StateChangeKind parse(String? raw) => switch (raw) {
    'services' => StateChangeKind.services,
    'clients' => StateChangeKind.clients,
    'server' => StateChangeKind.server,
    'config' => StateChangeKind.config,
    _ => StateChangeKind.unknown,
  };
}

/// Who made the change.
enum ChangeOrigin {
  /// This window.
  ui,

  /// A `pb_mapper_ui <verb>` in a terminal.
  cli,

  /// A background status probe noticing a transition.
  internal,
  unknown;

  static ChangeOrigin parse(String? raw) => switch (raw) {
    'ui' => ChangeOrigin.ui,
    'cli' => ChangeOrigin.cli,
    'internal' => ChangeOrigin.internal,
    _ => ChangeOrigin.unknown,
  };
}

/// An invalidation hint from Rust. Carries identity, not state: the receiver
/// re-reads through the normal API, so a dropped or coalesced event costs
/// nothing.
class StateChange {
  const StateChange({
    required this.kind,
    required this.origin,
    required this.seq,
    this.key,
  });

  final StateChangeKind kind;
  final ChangeOrigin origin;
  final int seq;

  /// The service this concerns, when it concerns just one.
  final String? key;

  factory StateChange.fromMap(Map<String, dynamic> map) => StateChange(
    kind: StateChangeKind.parse(map['kind'] as String?),
    origin: ChangeOrigin.parse(map['origin'] as String?),
    seq: (map['seq'] as num?)?.toInt() ?? 0,
    key: map['key'] as String?,
  );

  /// True for a change this window did not make, which is the case worth
  /// telling the user about.
  bool get isForeign => origin == ChangeOrigin.cli;
}

/// Reloads on [StateChange]s of the kinds a view cares about.
///
/// Coalesces a burst into one reload: starting three services produces three
/// events within a few milliseconds, and reloading three times would only make
/// the list flicker.
class ChangeSubscription {
  ChangeSubscription._(this._sub, this._timer);

  final StreamSubscription<StateChange> _sub;
  Timer? _timer;

  static const Duration _debounce = Duration(milliseconds: 150);

  static ChangeSubscription listen(
    Stream<StateChange> stream,
    Set<StateChangeKind> kinds,
    void Function(StateChange) onChange,
  ) {
    Timer? timer;
    late ChangeSubscription self;
    final sub = stream.listen((change) {
      // `unknown` always passes: a kind this build cannot name is still a
      // reason to re-read.
      if (!kinds.contains(change.kind) &&
          change.kind != StateChangeKind.unknown) {
        return;
      }
      timer?.cancel();
      timer = Timer(_debounce, () => onChange(change));
      self._timer = timer;
    });
    self = ChangeSubscription._(sub, timer);
    return self;
  }

  void cancel() {
    _timer?.cancel();
    _sub.cancel();
  }
}
