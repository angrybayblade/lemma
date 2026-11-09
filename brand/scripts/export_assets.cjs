/* Render standalone SVG masters to PNG with a local Chrome-compatible browser.
   PLAYWRIGHT_MODULE can be an absolute path to a playwright-core installation. */
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright-core');
const root = path.resolve(__dirname, '..');

async function exportAssets() {
  const browser = await chromium.launch({
    executablePath: process.env.CHROME_PATH || undefined,
    headless: true,
    args: ['--no-sandbox'],
  });
  try {
    const page = await browser.newPage({deviceScaleFactor: 1});
    const files = [
      ...fs.readdirSync(path.join(root, 'marketing')).filter(file => file.endsWith('.svg')).map(file => ['marketing', file]),
      ['logos', 'avatar.svg'], ['logos', 'avatar-dark.svg'], ['exports', 'brand-board.svg'],
    ];
    for (const [folder, filename] of files) {
      const source = path.join(root, folder, filename);
      const svg = fs.readFileSync(source, 'utf8');
      const width = Number(svg.match(/<svg[^>]*\bwidth="(\d+)"/)[1]);
      const height = Number(svg.match(/<svg[^>]*\bheight="(\d+)"/)[1]);
      await page.setViewportSize({width, height});
      await page.goto(pathToFileURL(source).href);
      await page.screenshot({path: path.join(root, 'exports', filename.replace('.svg', '.png')), omitBackground: true});
      console.log(`${filename} → ${width} × ${height} PNG`);
    }
    for (const [filename, size] of [['favicon-16.png', 16], ['favicon-32.png', 32], ['apple-touch-icon.png', 180]]) {
      const source = pathToFileURL(path.join(root, 'logos', 'favicon.svg')).href;
      await page.setViewportSize({width: size, height: size});
      await page.goto(source);
      await page.evaluate(size => {
        const svg = document.documentElement;
        svg.setAttribute('width', size);
        svg.setAttribute('height', size);
        svg.style.display = 'block';
      }, size);
      await page.screenshot({path: path.join(root, 'exports', filename), omitBackground: true});
      console.log(`${filename} → ${size} × ${size} PNG`);
    }
  } finally {
    await browser.close();
  }
}
exportAssets().catch(error => { console.error(error); process.exitCode = 1; });
