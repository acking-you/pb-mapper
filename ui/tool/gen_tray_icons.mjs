// Renders the Windows tray icons from their SVG sources into multi-size .ico
// files.
//
//   cd ui/tool && npm install
//   node gen_tray_icons.mjs
//
// Every size is rasterised from the vector at its own scale rather than
// resampled from a larger one, which is what keeps 16 and 20 legible.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';

// GetSystemMetricsForDpi(SM_CXSMICON, dpi) is 16 * dpi / 96, so these are the
// sizes Windows asks for across the 100..300% scaling steps. A missing entry
// means the shell stretches the nearest one with GDI, which does no filtering
// and looks visibly soft. 64 is kept as a master for anything else that wants
// a larger rendition.
const SIZES = [16, 20, 24, 28, 32, 36, 40, 48, 64];
const ICONS = ['tray_idle', 'tray_active', 'tray_offline'];

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = join(here, '..', 'assets', 'tray', 'src', 'win');
const outDir = join(here, '..', 'assets', 'tray');

function render(svg, size) {
  const resvg = new Resvg(svg, { fitTo: { mode: 'width', value: size } });
  return resvg.render().asPng();
}

// ICONDIR, then one ICONDIRENTRY per size, then the PNG payloads. PNG-compressed
// entries rather than DIBs; the shell has read those since Vista.
function buildIco(images) {
  const count = images.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type: icon
  header.writeUInt16LE(count, 4);

  const directory = Buffer.alloc(16 * count);
  let offset = 6 + 16 * count;
  images.forEach(({ size, data }, i) => {
    const at = i * 16;
    directory.writeUInt8(size >= 256 ? 0 : size, at + 0);
    directory.writeUInt8(size >= 256 ? 0 : size, at + 1);
    directory.writeUInt8(0, at + 2); // palette entries
    directory.writeUInt8(0, at + 3); // reserved
    directory.writeUInt16LE(0, at + 4); // colour planes
    directory.writeUInt16LE(32, at + 6); // bits per pixel
    directory.writeUInt32LE(data.length, at + 8);
    directory.writeUInt32LE(offset, at + 12);
    offset += data.length;
  });

  return Buffer.concat([header, directory, ...images.map((i) => i.data)]);
}

for (const name of ICONS) {
  const svg = readFileSync(join(srcDir, `${name}.svg`), 'utf8');
  const images = SIZES.map((size) => ({ size, data: render(svg, size) }));
  const ico = buildIco(images);
  writeFileSync(join(outDir, `${name}.ico`), ico);
  console.log(`${name}.ico  ${SIZES.join('/')}  ${ico.length} bytes`);
}
