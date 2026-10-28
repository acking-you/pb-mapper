/// SDK event data is preserved verbatim, including fields added by a newer
/// native library. Sequence numbers are scoped to one tunnel lifetime.
class TunnelDiagnosticEvent {
  TunnelDiagnosticEvent(this.data);
  final Map<String, dynamic> data;
  int get seq => (data['seq'] as num?)?.toInt() ?? 0;
  int get ageMs => (data['age_ms'] as num?)?.toInt() ?? 0;
  int get repeats => (data['repeats'] as num?)?.toInt() ?? 1;
  Map<String, dynamic> get detail =>
      Map<String, dynamic>.from(data['kind'] as Map? ?? const {});
  String get kind => detail['kind'] as String? ?? 'unknown';
  Map<String, dynamic>? get mapping => detail['mapping'] is Map
      ? Map<String, dynamic>.from(detail['mapping'] as Map)
      : null;
  String get stage =>
      (mapping?['stage'] ?? detail['stage'] ?? detail['phase'] ?? kind)
          .toString();
}

/// Initial snapshot, then individual transitions; lag and closure are explicit.
class DiagnosticUpdate {
  DiagnosticUpdate({
    this.snapshot,
    this.event,
    this.lagged = 0,
    this.closed = false,
  });
  final Map<String, dynamic>? snapshot;
  final TunnelDiagnosticEvent? event;
  final int lagged;
  final bool closed;

  factory DiagnosticUpdate.fromMap(Map<String, dynamic> map) =>
      DiagnosticUpdate(
        snapshot: map['snapshot'] is Map
            ? Map<String, dynamic>.from(map['snapshot'] as Map)
            : null,
        event: map['event'] is Map
            ? TunnelDiagnosticEvent(
                Map<String, dynamic>.from(map['event'] as Map),
              )
            : null,
        lagged: (map['lagged'] as num?)?.toInt() ?? 0,
        closed: map['closed'] == true,
      );
}
