// Vendor the canonical brand system so the Site source can build independently.
import {cpSync, mkdirSync, readdirSync, rmSync} from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const website = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.resolve(website, '../brand/system');
const destination = path.join(website, 'design-system');
// Read first: brand/system is ignored, so a fresh clone must fail before anything is deleted.
const entries = readdirSync(source, {withFileTypes:true});
rmSync(destination, {recursive:true, force:true});
mkdirSync(destination, {recursive:true});
for (const entry of entries) {
  if (entry.name.startsWith('.') || entry.name === '__pycache__') continue;
  cpSync(path.join(source, entry.name), path.join(destination, entry.name), {recursive:true});
}
console.log('Synced canonical brand/system to website/design-system.');
