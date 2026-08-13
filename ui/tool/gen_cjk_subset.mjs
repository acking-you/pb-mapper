// Cuts the bundled Chinese UI font down to the characters the app can
// plausibly show, one file per weight.
//
//   cd ui/tool && npm install
//   node gen_cjk_subset.mjs <path-to-NotoSansSC-VF.ttf>
//
// The source is the Noto Sans SC variable font (SIL Open Font License 1.1),
// from https://github.com/notofonts/noto-cjk/releases. It is not checked in:
// it is 17 MB, and only the subsets below are needed at build time.
//
// Static instances rather than the variable font itself, because Flutter never
// maps `fontWeight` onto the `wght` axis — grep the framework and the only
// `wght` writes are explicit `FontVariation`s in the icon widgets. Shipping the
// variable font would render every weight at its default instance, which for
// this font is 100 (Thin).

import { readFileSync, writeFileSync, mkdirSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import subsetFont from 'subset-font';
import iconv from 'iconv-lite';

// 400 and 500 are what Material asks for on its own (body, and titleMedium
// plus the three label sizes); 600 and 700 are this app's headings. Every one
// of them has to be a real face, or the shaper starts synthesising again and
// we are back to the problem this replaces.
const WEIGHTS = [
  { wght: 400, name: 'Regular' },
  { wght: 500, name: 'Medium' },
  { wght: 600, name: 'SemiBold' },
  { wght: 700, name: 'Bold' },
];

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, '..', 'assets', 'fonts');
const l10nDir = join(here, '..', 'lib', 'l10n');

function asciiAndPunctuation() {
  const chars = new Set();
  for (let c = 0x20; c <= 0x7e; c++) chars.add(String.fromCodePoint(c));
  // CJK punctuation, then the fullwidth forms that a Chinese keyboard emits.
  for (let c = 0x3000; c <= 0x303f; c++) chars.add(String.fromCodePoint(c));
  for (let c = 0xff00; c <= 0xff65; c++) chars.add(String.fromCodePoint(c));
  // Odds and ends that turn up in UI copy and log lines.
  for (const c of '·×÷°±—–…‘’“”←→↑↓•©®™§¥€£№≈≠≤≥∞') chars.add(c);
  return chars;
}

// Everything encodable in GB2312: 6763 hanzi, a well-defined line to draw and
// far cheaper than the 20992 of the CJK Unified Ideographs block. Anything
// outside it falls through to the system font, which is why the app still
// names a fallback after this family.
function gb2312Hanzi() {
  const chars = new Set();
  for (let hi = 0xb0; hi <= 0xf7; hi++) {
    for (let lo = 0xa1; lo <= 0xfe; lo++) {
      const decoded = iconv.decode(Buffer.from([hi, lo]), 'gb2312');
      if (decoded.length === 1 && decoded !== '�') chars.add(decoded);
    }
  }
  return chars;
}

// The app's own strings, so a translation that reaches for a rare character
// still renders in the bundled face rather than dropping to the fallback.
function localisedStrings() {
  const chars = new Set();
  for (const file of readdirSync(l10nDir).filter((f) => f.endsWith('.arb'))) {
    for (const c of readFileSync(join(l10nDir, file), 'utf8')) chars.add(c);
  }
  return chars;
}

const charset = new Set([
  ...asciiAndPunctuation(),
  ...gb2312Hanzi(),
  ...localisedStrings(),
]);
const text = [...charset].join('');

const sourcePath = process.argv[2];
if (!sourcePath) {
  console.error('usage: node gen_cjk_subset.mjs <path-to-NotoSansSC-VF.ttf>');
  process.exit(1);
}
const source = readFileSync(sourcePath);
mkdirSync(outDir, { recursive: true });

console.log(`charset: ${charset.size} characters`);
let total = 0;
for (const { wght, name } of WEIGHTS) {
  const subset = await subsetFont(source, text, {
    targetFormat: 'truetype',
    // Pins the axis, turning the variable font into a static instance that
    // Flutter's ordinary weight matching can select from pubspec.
    variationAxes: { wght },
    preserveNameIds: [1, 2, 3, 4, 6, 13, 14],
  });
  const out = join(outDir, `NotoSansSC-${name}.subset.ttf`);
  writeFileSync(out, subset);
  total += subset.length;
  console.log(
    `  wght ${wght} -> NotoSansSC-${name}.subset.ttf  ${(subset.length / 1024 / 1024).toFixed(2)} MB`,
  );
}
console.log(`total: ${(total / 1024 / 1024).toFixed(2)} MB`);
