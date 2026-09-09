// Generates src-tauri/app-icon.png (1024×1024): warm-black rounded square with
// the brass dot from the title bar. No dependencies; `tauri icon` derives the
// platform icon set from it. Replace with real artwork when available.
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, "../src-tauri/app-icon.png");
const S = 1024;

const BG = [0x0e, 0x0d, 0x0c];
const BRASS = [0xc9, 0xa9, 0x6a];
const HALO = [0x2b, 0x25, 0x1c];

function crc32(buf) {
  let c, crc = 0xffffffff;
  for (let n = 0; n < buf.length; n++) {
    c = (crc ^ buf[n]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

const raw = Buffer.alloc((S * 4 + 1) * S);
const cx = S / 2, cy = S / 2, r = S * 0.19, halo = S * 0.26, corner = S * 0.22;
for (let y = 0; y < S; y++) {
  raw[y * (S * 4 + 1)] = 0; // filter: none
  for (let x = 0; x < S; x++) {
    const i = y * (S * 4 + 1) + 1 + x * 4;
    // rounded-square mask
    const dx = Math.max(Math.abs(x - cx) - (S / 2 - corner), 0);
    const dy = Math.max(Math.abs(y - cy) - (S / 2 - corner), 0);
    const inside = Math.hypot(dx, dy) <= corner;
    const d = Math.hypot(x - cx, y - cy);
    let col = BG, a = inside ? 255 : 0;
    if (d <= r) col = BRASS;
    else if (d <= halo) {
      const t = (d - r) / (halo - r);
      col = HALO.map((h, k) => Math.round(h * (1 - t) + BG[k] * t));
    }
    raw[i] = col[0]; raw[i + 1] = col[1]; raw[i + 2] = col[2]; raw[i + 3] = a;
  }
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(S, 0); ihdr.writeUInt32BE(S, 4);
ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, png);
console.log(`wrote ${out}`);
