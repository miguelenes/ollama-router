import { readFileSync } from 'node:fs';
import sharp from 'sharp';

const svg = readFileSync('src/assets/banner.svg');
await sharp(svg, { density: 300 })
  .resize(1200, 630, { fit: 'contain', background: '#f4f1ea' })
  .png()
  .toFile('public/og.png');
console.log('wrote public/og.png');
