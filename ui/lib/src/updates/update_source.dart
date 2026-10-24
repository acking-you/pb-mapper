/// Where updates come from: GitHub's release listing, a release's signed
/// manifest, and its files.
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:ffi/ffi.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_ffi.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';

/// The user cancelled a download.
class UpdateCancelled implements Exception {
  const UpdateCancelled();
}

/// The network, behind an interface so tests need none.
abstract interface class UpdateHttp {
  /// The body at [url], refusing one larger than [limit] bytes.
  Future<List<int>> get(Uri url, {int limit = 1 << 20});

  /// Stream [url] into [target] and return the SHA-256 (lower-case hex) of
  /// what was written. Fails with [UpdateFailure.checksum] the moment more
  /// than [size] bytes arrive, or when fewer than [size] did.
  Future<String> download(
    Uri url,
    File target, {
    required int size,
    required void Function(int received) onProgress,
  });

  /// Stop a [download] in flight; it fails with [UpdateCancelled].
  void cancel();
}

/// [UpdateHttp] over `dart:io`, which follows GitHub's redirects to its
/// download host on its own.
class IoUpdateHttp implements UpdateHttp {
  HttpClient? _active;
  bool _cancelled = false;

  HttpClient _client() => HttpClient()
    ..connectionTimeout = const Duration(seconds: 15)
    ..userAgent = 'pb-mapper-updater';

  Future<HttpClientResponse> _open(HttpClient client, Uri url) async {
    final request = await client.getUrl(url);
    request.headers.set(
      HttpHeaders.acceptHeader,
      url.host == 'api.github.com' ? 'application/vnd.github+json' : '*/*',
    );
    final response = await request.close().timeout(const Duration(seconds: 30));
    if (response.statusCode != HttpStatus.ok) {
      await response.drain<void>().catchError((_) {});
      throw UpdateException(
        UpdateFailure.network,
        'HTTP ${response.statusCode} from $url',
      );
    }
    return response;
  }

  @override
  Future<List<int>> get(Uri url, {int limit = 1 << 20}) async {
    final client = _client();
    try {
      final response = await _open(client, url);
      final body = BytesBuilder(copy: false);
      await for (final chunk in response.timeout(const Duration(seconds: 30))) {
        body.add(chunk);
        if (body.length > limit) {
          throw UpdateException(
            UpdateFailure.network,
            '$url is larger than $limit bytes',
          );
        }
      }
      return body.takeBytes();
    } on UpdateException {
      rethrow;
    } catch (error) {
      throw UpdateException(UpdateFailure.network, '$error');
    } finally {
      client.close(force: true);
    }
  }

  @override
  Future<String> download(
    Uri url,
    File target, {
    required int size,
    required void Function(int received) onProgress,
  }) async {
    _cancelled = false;
    final client = _active = _client();
    final sink = target.openWrite();
    final digest = _DigestSink();
    final hasher = sha256.startChunkedConversion(digest);
    var received = 0;
    try {
      final response = await _open(client, url);
      await for (final chunk in response.timeout(const Duration(seconds: 60))) {
        if (_cancelled) throw const UpdateCancelled();
        received += chunk.length;
        if (received > size) {
          throw const UpdateException(
            UpdateFailure.checksum,
            'The download is larger than the release lists',
          );
        }
        hasher.add(chunk);
        sink.add(chunk);
        onProgress(received);
      }
      if (received != size) {
        throw UpdateException(
          UpdateFailure.checksum,
          'Received $received of $size bytes',
        );
      }
      hasher.close();
      return '${digest.value}';
    } on UpdateException {
      rethrow;
    } on UpdateCancelled {
      rethrow;
    } catch (error) {
      if (_cancelled) throw const UpdateCancelled();
      throw UpdateException(UpdateFailure.network, '$error');
    } finally {
      _active = null;
      await sink.close().catchError((_) {});
      client.close(force: true);
    }
  }

  @override
  void cancel() {
    _cancelled = true;
    _active?.close(force: true);
  }
}

class _DigestSink implements Sink<Digest> {
  Digest? value;

  @override
  void add(Digest data) => value = data;

  @override
  void close() {}
}

/// Checks an Ed25519 signature, behind an interface so tests need no
/// native library.
abstract interface class SignatureVerifier {
  bool verify(List<int> message, List<int> signature, List<int> publicKey);
}

typedef _VerifyNative =
    Int32 Function(
      Pointer<Uint8>,
      Uint32,
      Pointer<Uint8>,
      Uint32,
      Pointer<Uint8>,
      Uint32,
    );
typedef _VerifyDart =
    int Function(Pointer<Uint8>, int, Pointer<Uint8>, int, Pointer<Uint8>, int);

/// [SignatureVerifier] through `pb_mapper_ed25519_verify_v1` in the app's
/// FFI library (or [library]), which checks it with `ring`.
class NativeSignatureVerifier implements SignatureVerifier {
  NativeSignatureVerifier([this.library]);

  final DynamicLibrary? library;

  late final _VerifyDart _verify = (library ?? PbMapperFFI().lib)
      .lookupFunction<_VerifyNative, _VerifyDart>(
        'pb_mapper_ed25519_verify_v1',
        isLeaf: true,
      );

  @override
  bool verify(List<int> message, List<int> signature, List<int> publicKey) {
    Pointer<Uint8> copy(List<int> bytes) {
      // One byte at least, so an empty message still has an address.
      final buffer = malloc<Uint8>(bytes.isEmpty ? 1 : bytes.length);
      buffer.asTypedList(bytes.length).setAll(0, bytes);
      return buffer;
    }

    final m = copy(message);
    final s = copy(signature);
    final k = copy(publicKey);
    try {
      return _verify(
            m,
            message.length,
            s,
            signature.length,
            k,
            publicKey.length,
          ) ==
          1;
    } finally {
      malloc
        ..free(m)
        ..free(s)
        ..free(k);
    }
  }
}

/// The releases on GitHub, and the trust check between them and the app.
class UpdateSource {
  UpdateSource({
    UpdateHttp? http,
    SignatureVerifier? verifier,
    this.publicKey = updatePublicKey,
  }) : http = http ?? IoUpdateHttp(),
       _verifier = verifier;

  final UpdateHttp http;
  final String publicKey;
  final SignatureVerifier? _verifier;
  late final SignatureVerifier verifier =
      _verifier ?? NativeSignatureVerifier();

  static final Uri releasesUrl = Uri.https(
    'api.github.com',
    '/repos/$updateRepository/releases',
    {'per_page': '30'},
  );

  /// The newest published UI release, or null when there is none.
  Future<PublishedRelease?> newest() async {
    final body = await http.get(releasesUrl, limit: 8 << 20);
    final Object? listing;
    try {
      listing = jsonDecode(utf8.decode(body));
    } on FormatException catch (error) {
      throw UpdateException(UpdateFailure.network, error.message);
    }
    if (listing is! List) {
      throw const UpdateException(
        UpdateFailure.network,
        'GitHub did not answer with a list of releases',
      );
    }
    return PublishedRelease.newest(listing);
  }

  /// [release]'s manifest, once its signature is shown to be the release
  /// key's and it is shown to describe [release] itself.
  Future<ReleaseManifest> manifest(PublishedRelease release) async {
    final manifestUrl = release.files[manifestName];
    final signatureUrl = release.files[signatureName];
    if (manifestUrl == null || signatureUrl == null) {
      throw const UpdateException(UpdateFailure.unsigned);
    }
    final manifest = await http.get(manifestUrl);
    final signatureFile = await http.get(signatureUrl, limit: 4096);
    final List<int> signature;
    try {
      signature = base64Decode(utf8.decode(signatureFile).trim());
    } on FormatException catch (error) {
      throw UpdateException(UpdateFailure.signature, error.message);
    }
    if (!verifier.verify(manifest, signature, base64Decode(publicKey))) {
      throw const UpdateException(
        UpdateFailure.signature,
        'The manifest is not signed by the release key',
      );
    }
    final ReleaseManifest parsed;
    try {
      parsed = ReleaseManifest.parse(utf8.decode(manifest));
    } on FormatException catch (error) {
      throw UpdateException(UpdateFailure.signature, error.message);
    }
    // A genuinely signed manifest of an older release must not stand in for
    // this one's.
    if (parsed.version != release.version) {
      throw UpdateException(
        UpdateFailure.signature,
        'The manifest describes ${parsed.version}, the release is '
        '${release.version}',
      );
    }
    return parsed;
  }
}
