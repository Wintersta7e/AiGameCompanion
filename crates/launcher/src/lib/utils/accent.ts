/**
 * Per-game accent extraction — the "cover art drives the color" idea.
 *
 * `dominantAccent` downscales the cover image to a tiny canvas, averages the
 * vibrant pixels (weighted by saturation), and normalises the result to a
 * pleasant accent (consistent lightness/chroma). If the image can't be read
 * (cross-origin taint on Steam-CDN art, missing file, etc.) the caller should
 * fall back to `hashHue`.
 *
 * NOTE: Steam CDN images are cross-origin. For getImageData to work without
 * tainting the canvas, either (a) proxy/cache cover art locally in the Tauri
 * backend and serve via convertFileSrc, or (b) accept the hashed-hue fallback.
 */

export function hashHue(seed: string): string {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  // oklch keeps lightness/chroma constant across games -> harmonious palette
  return `oklch(0.74 0.15 ${hue})`;
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = 'anonymous';
    img.onload = () => resolve(img);
    img.onerror = reject;
    img.src = src;
  });
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b),
    min = Math.min(r, g, b);
  const l = (max + min) / 2;
  let h = 0,
    s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    switch (max) {
      case r:
        h = (g - b) / d + (g < b ? 6 : 0);
        break;
      case g:
        h = (b - r) / d + 2;
        break;
      default:
        h = (r - g) / d + 4;
        break;
    }
    h /= 6;
  }
  return [h * 360, s, l];
}

export async function dominantAccent(src: string): Promise<string> {
  const img = await loadImage(src);
  const w = 48;
  const h = Math.max(1, Math.round((48 * img.height) / Math.max(1, img.width)));
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) throw new Error('no 2d context');
  ctx.drawImage(img, 0, 0, w, h);
  const { data } = ctx.getImageData(0, 0, w, h);

  let r = 0,
    g = 0,
    b = 0,
    n = 0;
  for (let i = 0; i < data.length; i += 4) {
    const R = data[i],
      G = data[i + 1],
      B = data[i + 2];
    const max = Math.max(R, G, B),
      min = Math.min(R, G, B);
    const sat = max === 0 ? 0 : (max - min) / max;
    const lum = (max + min) / 2;
    if (sat > 0.22 && lum > 38 && lum < 232) {
      const wt = sat * sat; // favour the most saturated pixels
      r += R * wt;
      g += G * wt;
      b += B * wt;
      n += wt;
    }
  }
  if (n === 0) throw new Error('no vibrant pixels');
  const [hue] = rgbToHsl(r / n, g / n, b / n);
  // normalise to a consistent, comfortable accent regardless of source vividness
  return `oklch(0.74 0.155 ${Math.round(hue)})`;
}
