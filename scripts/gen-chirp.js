// Tether notify sound = variant d. Regenerate: node scripts/gen-chirp.js d (writes chirp-d.wav to cwd), then copy it to daemon/assets/notify.wav
// Procedural bird-chirp notification generator. No deps. Usage: node gen-chirp.js [a|b|c ...]
// Each syllable: t = start (s), d = duration (s), f = freq points (Hz, log-interpolated),
// amp, atk = attack fraction, vib = [rateHz, depthRatio], h2 = 2nd harmonic level.
import fs from 'node:fs';
import path from 'node:path';
const SR = 44100;

const VARIANTS = {
  // (a) bright double "tweet-tweet": two rising chirps
  a: {
    len: 0.55, lp: 6000, verb: 0.12,
    syl: [
      { t: 0.010, d: 0.085, f: [2500, 4800, 4500], amp: 1.0, atk: 0.15, vib: [30, 0.012], h2: 0.10 },
      { t: 0.180, d: 0.095, f: [2600, 5000, 4600], amp: 0.9, atk: 0.15, vib: [30, 0.012], h2: 0.10 },
    ],
  },
  // (b) warbling trill: 7 short down-swept notes, descending overall
  b: {
    len: 0.62, lp: 5800, verb: 0.15,
    syl: Array.from({ length: 7 }, (_, i) => {
      const fc = 4600 * Math.pow(3000 / 4600, i / 6);
      return { t: 0.010 + i * 0.062, d: 0.045, f: [fc * 1.22, fc, fc * 0.86], amp: 1 - i * 0.06,
               atk: 0.2, vib: [36, 0.015], h2: 0.08 };
    }),
  },
  // (c) gentle "chip-chee-wee": softer, lower (2-3.5 kHz)
  c: {
    len: 0.80, lp: 4500, verb: 0.18,
    syl: [
      { t: 0.015, d: 0.055, f: [2600, 3100, 2800], amp: 0.75, atk: 0.3, vib: [24, 0.008], h2: 0.12 },
      { t: 0.130, d: 0.100, f: [2900, 3300, 3200], amp: 0.85, atk: 0.35, vib: [24, 0.010], h2: 0.12 },
      { t: 0.300, d: 0.200, f: [2200, 2500, 3400], amp: 1.0, atk: 0.35, vib: [22, 0.012], h2: 0.12 },
    ],
  },
  // (d) = c reversed: three notes ascending low -> high, each gliding upward
  d: {
    len: 0.80, lp: 4500, verb: 0.18,
    syl: [
      { t: 0.015, d: 0.090, f: [2000, 2200, 2400], amp: 0.75, atk: 0.3, vib: [24, 0.008], h2: 0.12 },
      { t: 0.170, d: 0.100, f: [2400, 2650, 2850], amp: 0.85, atk: 0.35, vib: [24, 0.010], h2: 0.12 },
      { t: 0.340, d: 0.180, f: [2800, 3100, 3450], amp: 1.0, atk: 0.35, vib: [22, 0.012], h2: 0.12 },
    ],
  },
};

// log-domain piecewise interpolation of freq points over p in [0,1]
function freqAt(pts, p) {
  const seg = Math.min(pts.length - 2, Math.floor(p * (pts.length - 1)));
  const q = p * (pts.length - 1) - seg;
  return pts[seg] * Math.pow(pts[seg + 1] / pts[seg], q);
}

function render(v) {
  const n = Math.round(v.len * SR);
  const x = new Float64Array(n);
  for (const s of v.syl) {
    const i0 = Math.round(s.t * SR), m = Math.round(s.d * SR);
    let ph = 0;
    for (let k = 0; k < m && i0 + k < n; k++) {
      const p = k / m, t = k / SR;
      const f = freqAt(s.f, p) * (1 + s.vib[1] * Math.sin(2 * Math.PI * s.vib[0] * t));
      ph += 2 * Math.PI * f / SR;
      // quick raised-cosine attack, exponential-ish decay
      const env = p < s.atk ? 0.5 - 0.5 * Math.cos(Math.PI * p / s.atk)
                            : Math.pow(Math.cos(0.5 * Math.PI * (p - s.atk) / (1 - s.atk)), 1.5);
      x[i0 + k] += s.amp * env * (Math.sin(ph) + s.h2 * Math.sin(2 * ph));
    }
  }
  // subtle reverb: 3 feedback combs, mixed in lightly
  const wet = new Float64Array(n);
  for (const [ms, g] of [[23, 0.45], [31, 0.4], [41, 0.35]]) {
    const D = Math.round(ms * SR / 1000), buf = new Float64Array(n);
    for (let i = 0; i < n; i++) { buf[i] = x[i] + (i >= D ? g * buf[i - D] : 0); wet[i] += buf[i] / 3; }
  }
  for (let i = 0; i < n; i++) x[i] += v.verb * wet[i];
  // 2x one-pole low-pass to tame energy above ~6 kHz
  const a = Math.exp(-2 * Math.PI * v.lp / SR);
  for (let pass = 0; pass < 2; pass++) { let y = 0; for (let i = 0; i < n; i++) x[i] = y = (1 - a) * x[i] + a * y; }
  // DC removal, 5 ms fade in / 30 ms fade out, normalize to -3 dBFS
  const mean = x.reduce((s, y) => s + y, 0) / n;
  const fi = Math.round(0.005 * SR), fo = Math.round(0.03 * SR);
  let peak = 0;
  for (let i = 0; i < n; i++) {
    x[i] -= mean;
    if (i < fi) x[i] *= i / fi;
    if (i >= n - fo) x[i] *= (n - 1 - i) / fo;
    peak = Math.max(peak, Math.abs(x[i]));
  }
  const gain = Math.pow(10, -3 / 20) / peak;
  return x.map(y => y * gain);
}

function writeWav(file, x) {
  const n = x.length, b = Buffer.alloc(44 + n * 2);
  b.write('RIFF', 0); b.writeUInt32LE(36 + n * 2, 4); b.write('WAVE', 8);
  b.write('fmt ', 12); b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(SR, 24); b.writeUInt32LE(SR * 2, 28); b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34);
  b.write('data', 36); b.writeUInt32LE(n * 2, 40);
  for (let i = 0; i < n; i++) b.writeInt16LE(Math.max(-32768, Math.min(32767, Math.round(x[i] * 32767))), 44 + i * 2);
  fs.writeFileSync(file, b);
}

const pick = process.argv.slice(2);
for (const k of pick.length ? pick : Object.keys(VARIANTS)) {
  const file = path.resolve(`chirp-${k}.wav`);
  writeWav(file, render(VARIANTS[k]));
  console.log('wrote', file);
}
