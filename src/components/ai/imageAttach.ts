// ============================================================================
// Image attachment: compression and conversion
//
// **Why compression is mandatory**: images are put into requests as base64, ~1/3 larger than the original file;
// and conversations are stateless — the whole history **is resent every turn**. A 2MB screenshot adds 2.7MB to every request,
// after a few turns it is both slow and expensive. Compress to long edge 1280 / quality 0.8, usually landing at 200~400KB.
//
// Use canvas instead of pulling in an image library: the only need is "resize + convert to JPEG",
// native browser APIs are fully sufficient, and it follows the project convention "no new dependency if native will do".
//
// ⚠️ Always output JPEG: screenshots have no transparency channel, JPEG is much smaller.
//    The cost is that images with transparent backgrounds become black/white — acceptable for the current use (screenshot questions).
// ============================================================================

/** Compressed image attachment, ready to send to the backend. */
export interface AttachedImage {
  /** e.g. image/jpeg */
  mediaType: string;
  /** Pure base64 without the data: prefix */
  base64: string;
  /** For thumbnail preview in the UI */
  previewUrl: string;
}

/** Long-edge limit: any larger does not help "understanding the screenshot", only makes each request heavier. */
const MAX_EDGE = 1280;
const JPEG_QUALITY = 0.8;

/** Per-image limit: reject above this, to avoid users accidentally picking an image of tens of MB. */
export const MAX_SOURCE_BYTES = 20 * 1024 * 1024;

/**
 * Compress an image to a sendable size.
 * Throw on failure (not an image, cannot decode); the caller handles the prompt — do not silently drop.
 */
export async function compressImage(file: File): Promise<AttachedImage> {
  if (!file.type.startsWith('image/')) {
    throw new Error('不是图片文件');
  }
  if (file.size > MAX_SOURCE_BYTES) {
    throw new Error('图片太大，请先裁剪或压缩后再试');
  }

  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, MAX_EDGE / Math.max(bitmap.width, bitmap.height));
  const width = Math.max(1, Math.round(bitmap.width * scale));
  const height = Math.max(1, Math.round(bitmap.height * scale));

  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext('2d');
  if (!context) {
    throw new Error('无法处理图片');
  }
  // White background: JPEG has no transparency channel, drawing directly would leave black blocks in transparent areas
  context.fillStyle = '#ffffff';
  context.fillRect(0, 0, width, height);
  context.drawImage(bitmap, 0, 0, width, height);
  bitmap.close();

  const dataUrl = canvas.toDataURL('image/jpeg', JPEG_QUALITY);
  const base64 = dataUrl.split(',')[1] ?? '';
  if (!base64) {
    throw new Error('图片编码失败');
  }

  return { mediaType: 'image/jpeg', base64, previewUrl: dataUrl };
}

/** Extract an image file from the clipboard or file picker. */
export function collectImageFiles(list: FileList | File[] | null | undefined): File[] {
  if (!list) return [];
  return Array.from(list).filter((file) => file.type.startsWith('image/'));
}
