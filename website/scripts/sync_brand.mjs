// Refresh the canonical design-system copy and the standalone mark assets.
import './sync_design_system.mjs';
import {cpSync} from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
for(const file of ['mark.svg','favicon.svg'])cpSync(path.join(root,'design-system/assets/mark.svg'),path.join(root,'assets/logos',file));
console.log('Synchronized the faded metallic design system and mark assets. Regenerate landing artwork after palette changes.');
