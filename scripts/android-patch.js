// Допълва AndroidManifest.xml на генерирания Android проект:
//  - CAMERA: сканиране на QR кода от компютъра
//  - usesCleartextTraffic: връзка с компютъра по локалната мрежа (http://192.168.x.x)
// Може да се пуска много пъти: не добавя нищо два пъти.
const fs = require('fs'), path = require('path');
const f = path.join(__dirname, '..', 'android', 'app', 'src', 'main', 'AndroidManifest.xml');
if (!fs.existsSync(f)) { console.warn('Няма android проект, пропускам'); process.exit(0); }
let x = fs.readFileSync(f, 'utf8');
const before = x;
const add = (needle, line) => {
  if (x.includes(needle)) return;
  const i = x.indexOf('<application');
  x = x.slice(0, i) + line + '\n    ' + x.slice(i);
};
add('android.permission.CAMERA', '<uses-permission android:name="android.permission.CAMERA" />');
add('android.permission.INTERNET', '<uses-permission android:name="android.permission.INTERNET" />');
add('android.hardware.camera"', '<uses-feature android:name="android.hardware.camera" android:required="false" />');
if (!/usesCleartextTraffic/.test(x)) x = x.replace('<application', '<application android:usesCleartextTraffic="true"');
if (x !== before) { fs.writeFileSync(f, x); console.log('AndroidManifest.xml е допълнен'); }
else console.log('AndroidManifest.xml вече е наред');
