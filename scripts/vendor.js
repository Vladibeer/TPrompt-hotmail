// Копира библиотеките в www/vendor, за да работят без интернет:
//  - mammoth: отваряне на .docx
//  - jsQR: четене на QR код с камерата (дистанционно в Android)
const fs = require('fs'), path = require('path');
const root = path.join(__dirname, '..');
const dst = path.join(root, 'www', 'vendor');
const files = [
  ['mammoth', path.join('node_modules', 'mammoth', 'mammoth.browser.min.js'), 'mammoth.browser.min.js'],
  ['jsQR', path.join('node_modules', 'jsqr', 'dist', 'jsQR.js'), 'jsQR.js'],
];
fs.mkdirSync(dst, { recursive: true });
for (const [name, rel, out] of files) {
  const src = path.join(root, rel);
  if (!fs.existsSync(src)) { console.warn(name + ' не е инсталиран, пропускам'); continue; }
  fs.copyFileSync(src, path.join(dst, out));
  console.log(name + ' -> www/vendor/' + out);
}
