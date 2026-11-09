# pb-mapper-desktop

Encrypted remote desktop over a pb-mapper deployment: a host shares a
display behind a secret invitation, and a viewer connects to it on the
local network first, then a direct path, then the relay. TLS 1.3 pins the
host's certificate from the invitation; control is granted at the host and
requested by the viewer, separately from viewing.

Most applications use it through the `pb-mapper` crate's `desktop` feature,
as `pb_mapper::desktop`.

- `Host::builder(HostOptions::new("desktop:…"))` shares this computer's
  display, or an embedder's own capture through `.backend(..)`.
- `Viewer::connect` receives Annex B H.264/H.265 access units or JPEG; the
  application brings the decoder and reports each frame it presents.
- `host_support()` says whether this computer can host, and why not.

Hosting is implemented on Windows (DXGI desktop duplication, a D3D11 video
processor and the GPU's Media Foundation encoder; an optional, separately
authorized helper service reaches UAC and the lock screen). Viewing works
everywhere. Other platforms plug in behind the same `Platform` seam without
changing the engine, the protocol or the viewer: see "Platforms" in
[`docs/remote-desktop.md`](../../docs/remote-desktop.md).
