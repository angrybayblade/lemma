// Render the landing page and sharing image with local Chrome and Playwright.
const path = require('node:path');
const {mkdirSync} = require('node:fs');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright-core');
const root = path.resolve(__dirname, '..');
const base = process.env.LEMMA_PREVIEW_URL || 'http://127.0.0.1:8765/';
(async()=>{
 const browser = await chromium.launch({executablePath:process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
 try {
  mkdirSync(path.join(root,'previews'),{recursive:true});
  const desktop=await browser.newPage({viewport:{width:1440,height:1000},deviceScaleFactor:1});
  await desktop.goto(base,{waitUntil:'networkidle'});
  await desktop.screenshot({path:path.join(root,'previews/desktop.png')});
  await desktop.getByRole('button',{name:'Pause motion',exact:true}).click();
  await desktop.screenshot({path:path.join(root,'previews/full-page.png'),fullPage:true});
  await desktop.close();
  const mobile=await browser.newPage({viewport:{width:390,height:844},deviceScaleFactor:1});
  await mobile.goto(base,{waitUntil:'networkidle'});
  await mobile.screenshot({path:path.join(root,'previews/mobile.png')});
  await mobile.close();
  const social=await browser.newPage({viewport:{width:1200,height:630},deviceScaleFactor:1});
  await social.goto(`${base}assets/social-card.svg`,{waitUntil:'networkidle'});
  await social.screenshot({path:path.join(root,'assets/social-card.png')});
  console.log('Exported desktop, full-page, mobile, and Open Graph previews.');
 }finally{await browser.close();}
})().catch(error=>{console.error(error.message);process.exitCode=1;});
