/**
 * NSIS installer image generation script
 * Uses the sharp library to generate the BMP images required by NSIS
 * 
 * Note: sharp doesn't support BMP output directly, so the BMP file is built manually here
 * 
 * Usage:
 * node scripts/gen-nsis-assets.js
 */

import sharp from 'sharp';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const iconsDir = path.join(__dirname, '../src-tauri/icons');
const nsisDir = path.join(__dirname, '../src-tauri/nsis');
const svgPath = path.join(iconsDir, 'icon.svg');

// Ensure the nsis directory exists
if (!fs.existsSync(nsisDir)) {
  fs.mkdirSync(nsisDir, { recursive: true });
  console.log('✓ 创建目录: src-tauri/nsis');
}

/**
 * Convert an RGB raw buffer into a 24-bit BMP file
 * @param {Buffer} rawBuffer - RGB raw data (3 bytes per pixel)
 * @param {number} width - image width
 * @param {number} height - image height
 * @returns {Buffer} - BMP file data
 */
function rgbToBmp(rawBuffer, width, height) {
  // BMP rows must be aligned to 4 bytes
  const rowSize = Math.ceil((width * 3) / 4) * 4;
  const padding = rowSize - width * 3;
  const imageSize = rowSize * height;
  const fileSize = 54 + imageSize; // 14 (file header) + 40 (info header) + image data

  const bmp = Buffer.alloc(fileSize);
  let offset = 0;

  // BMP File Header (14 bytes)
  bmp.write('BM', offset); offset += 2;           // Signature
  bmp.writeUInt32LE(fileSize, offset); offset += 4; // File size
  bmp.writeUInt16LE(0, offset); offset += 2;      // Reserved
  bmp.writeUInt16LE(0, offset); offset += 2;      // Reserved
  bmp.writeUInt32LE(54, offset); offset += 4;     // Pixel data offset

  // BMP Info Header (40 bytes)
  bmp.writeUInt32LE(40, offset); offset += 4;     // Header size
  bmp.writeInt32LE(width, offset); offset += 4;   // Width
  bmp.writeInt32LE(height, offset); offset += 4;  // Height (positive = bottom-up)
  bmp.writeUInt16LE(1, offset); offset += 2;      // Planes
  bmp.writeUInt16LE(24, offset); offset += 2;     // Bits per pixel
  bmp.writeUInt32LE(0, offset); offset += 4;      // Compression (0 = none)
  bmp.writeUInt32LE(imageSize, offset); offset += 4; // Image size
  bmp.writeInt32LE(2835, offset); offset += 4;    // X pixels per meter
  bmp.writeInt32LE(2835, offset); offset += 4;    // Y pixels per meter
  bmp.writeUInt32LE(0, offset); offset += 4;      // Colors used
  bmp.writeUInt32LE(0, offset); offset += 4;      // Important colors

  // Pixel data (bottom-up, BGR order)
  for (let y = height - 1; y >= 0; y--) {
    for (let x = 0; x < width; x++) {
      const srcIdx = (y * width + x) * 3;
      const r = rawBuffer[srcIdx];
      const g = rawBuffer[srcIdx + 1];
      const b = rawBuffer[srcIdx + 2];
      // BMP uses BGR order
      bmp[offset++] = b;
      bmp[offset++] = g;
      bmp[offset++] = r;
    }
    // Add padding
    for (let p = 0; p < padding; p++) {
      bmp[offset++] = 0;
    }
  }

  return bmp;
}

/**
 * Generate wizard.bmp (164x314)
 * Left-side wizard image, vertical gradient background from emerald green to dark, Logo centered in the upper half
 */
async function generateWizardBmp() {
  const width = 164;
  const height = 314;
  const logoSize = 100;
  const logoY = 60; // Logo in the upper half

  // Create the vertical gradient background SVG
  const gradientSvg = `
    <svg width="${width}" height="${height}" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="wizardGradient" x1="0%" y1="0%" x2="0%" y2="100%">
          <stop offset="0%" style="stop-color:#00b96b"/>
          <stop offset="100%" style="stop-color:#061a12"/>
        </linearGradient>
      </defs>
      <rect width="${width}" height="${height}" fill="url(#wizardGradient)"/>
    </svg>
  `;

  // Generate the gradient background
  const background = await sharp(Buffer.from(gradientSvg))
    .png()
    .toBuffer();

  // Read and resize the Logo
  let logoBuffer;
  if (fs.existsSync(svgPath)) {
    const svgContent = fs.readFileSync(svgPath);
    logoBuffer = await sharp(svgContent)
      .resize(logoSize, logoSize)
      .png()
      .toBuffer();
  } else {
    // If there's no SVG, create a simple "C:" text Logo
    const fallbackSvg = `
      <svg width="${logoSize}" height="${logoSize}" xmlns="http://www.w3.org/2000/svg">
        <rect width="${logoSize}" height="${logoSize}" rx="20" fill="#07C160"/>
        <text x="50%" y="60%" font-family="Arial" font-size="48" font-weight="bold" fill="white" text-anchor="middle">C:</text>
      </svg>
    `;
    logoBuffer = await sharp(Buffer.from(fallbackSvg))
      .png()
      .toBuffer();
  }

  // Composite: background + Logo
  const logoX = Math.floor((width - logoSize) / 2);
  
  // Get the RGB raw data
  const rawBuffer = await sharp(background)
    .composite([
      {
        input: logoBuffer,
        left: logoX,
        top: logoY
      }
    ])
    .removeAlpha()
    .raw()
    .toBuffer();

  // Convert to BMP and save
  const bmpBuffer = rgbToBmp(rawBuffer, width, height);
  fs.writeFileSync(path.join(nsisDir, 'wizard.bmp'), bmpBuffer);

  console.log('✓ 生成 wizard.bmp (164x314) - 向导侧边栏图片');
}

/**
 * Generate header.bmp (150x57)
 * Top banner image, white background, with a simplified Logo on the right
 */
async function generateHeaderBmp() {
  const width = 150;
  const height = 57;
  const logoSize = 40;

  // Create the white background
  const backgroundSvg = `
    <svg width="${width}" height="${height}" xmlns="http://www.w3.org/2000/svg">
      <rect width="${width}" height="${height}" fill="white"/>
    </svg>
  `;

  const background = await sharp(Buffer.from(backgroundSvg))
    .png()
    .toBuffer();

  // Read and resize the Logo
  let logoBuffer;
  if (fs.existsSync(svgPath)) {
    const svgContent = fs.readFileSync(svgPath);
    logoBuffer = await sharp(svgContent)
      .resize(logoSize, logoSize)
      .png()
      .toBuffer();
  } else {
    // If there's no SVG, create a simple Logo
    const fallbackSvg = `
      <svg width="${logoSize}" height="${logoSize}" xmlns="http://www.w3.org/2000/svg">
        <rect width="${logoSize}" height="${logoSize}" rx="8" fill="#07C160"/>
        <text x="50%" y="65%" font-family="Arial" font-size="20" font-weight="bold" fill="white" text-anchor="middle">C:</text>
      </svg>
    `;
    logoBuffer = await sharp(Buffer.from(fallbackSvg))
      .png()
      .toBuffer();
  }

  // Place the Logo on the right, vertically centered
  const logoX = width - logoSize - 8;
  const logoY = Math.floor((height - logoSize) / 2);

  // Get the RGB raw data
  const rawBuffer = await sharp(background)
    .composite([
      {
        input: logoBuffer,
        left: logoX,
        top: logoY
      }
    ])
    .removeAlpha()
    .raw()
    .toBuffer();

  // Convert to BMP and save
  const bmpBuffer = rgbToBmp(rawBuffer, width, height);
  fs.writeFileSync(path.join(nsisDir, 'header.bmp'), bmpBuffer);

  console.log('✓ 生成 header.bmp (150x57) - 安装向导顶部横幅');
}

async function main() {
  console.log('========================================');
  console.log('  NSIS 安装包图片生成脚本');
  console.log('========================================\n');

  try {
    await generateWizardBmp();
    await generateHeaderBmp();
    
    console.log('\n========================================');
    console.log('  所有 NSIS 图片生成完成！');
    console.log('  输出目录: src-tauri/nsis/');
    console.log('========================================');
  } catch (error) {
    console.error('生成失败:', error);
    process.exit(1);
  }
}

main();
