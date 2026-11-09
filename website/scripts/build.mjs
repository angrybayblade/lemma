// Static build with a vendored build-time Markdown parser. Only public assets enter the output.
import {cpSync, mkdirSync, rmSync, readFileSync, writeFileSync} from 'node:fs';
import path from 'node:path';
import {buildDocs} from './build_docs.mjs';
import {highlightLm} from './highlight_lm.mjs';
import {fileURLToPath} from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const output = path.join(root, 'dist');
rmSync(output, {recursive: true, force: true});
mkdirSync(output, {recursive: true});
for (const file of ['styles.css', 'app.js', 'diagrams-webgl.js', 'change-story.js']) cpSync(path.join(root, 'src', file), path.join(output, file));
let html = readFileSync(path.join(root, 'src/index.html'), 'utf8');
const story = JSON.parse(readFileSync(path.join(root, 'assets/change-story.json'), 'utf8'));
const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
function renderSnippet(source, filename, caption, label, kind = 'snippet') {
  const copy = escape(source).replaceAll('\n', '&#10;');
  return `<figure class="config-example" aria-label="${escape(label)}"><div class="config-heading"><span>${escape(filename)}</span><button type="button" data-copy-kind="${kind}" data-copy="${copy}" aria-label="Copy ${escape(label)}"><svg class="lm-icon" aria-hidden="true"><use href="design-system/assets/symbols.svg#copy"/></svg></button></div><pre><code>${filename.endsWith('.lm') ? highlightLm(source) : escape(source)}</code></pre><figcaption>${escape(caption)}</figcaption></figure>`;
}
const COPY_ICON = '<svg class="lm-icon" aria-hidden="true"><use href="design-system/assets/symbols.svg#copy"/></svg>';
function renderFileTabs(files, caption, label) {
  const tabs = files.map(([filename], index) => `<button type="button" role="tab" id="file-tab-${index}" aria-controls="file-panel-${index}" aria-selected="${index === 0}" tabindex="${index === 0 ? 0 : -1}">${escape(filename)}</button>`).join('');
  const panels = files.map(([, source], index) => `<pre role="tabpanel" id="file-panel-${index}" aria-labelledby="file-tab-${index}" tabindex="0"${index === 0 ? '' : ' hidden'}><code>${highlightLm(source)}</code></pre>`).join('');
  const firstSource = escape(files[0][1]).replaceAll('\n', '&#10;');
  return `<div class="file-tabs" data-file-tabs><div class="example-switch" role="tablist" aria-label="${escape(label)}">${tabs}</div><figure class="config-example" aria-label="${escape(label)}"><div class="config-heading"><button type="button" data-copy-kind="config" data-copy="${firstSource}" aria-label="Copy the selected file">${COPY_ICON}</button></div><div class="file-panels">${panels}</div><figcaption>${escape(caption)}</figcaption></figure></div>`;
}
// Verbatim from docs/quickstart.md, whose output the docs tests run and compare.
const finding = [
  'error   #web:web:src/charge.ts::charge calls POST /v1/charges, which now requires `currency`; its code never names `currency` (#web:web:src/charge.ts:2)',
  '        [consumer_misses_new_required_field] #web:web:src/charge.ts::charge -> POST /v1/charges  (introduced)',
  'warning #web:web:src/charge.ts::charge calls POST /v1/charges and names `receipt_url`; response field `receipt_url` was removed or renamed (#web:web:src/charge.ts:2)',
  '        [consumer_uses_removed_response_field] #web:web:src/charge.ts::charge -> POST /v1/charges  (introduced)',
  '1 errors, 1 warnings, 0 informational',
].join('\n');
// Every file the finding above needs, verbatim from docs/quickstart.md.
const setupFiles = [
  ['billing/mod.lm', `module billing

import * as http from lemma.http

export service api {
    operations: http.openapi("openapi.json")
}`],
  ['web/mod.lm', `module web

export service web {
    index: { include: ["src"] }
}`],
  ['mod.lm', `module shop

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}`],
];
// Verbatim from docs/tutorials/2-requirements.md.
const requirement = `import { calls } from lemma.http

relation Deprecated(operation: Text)

graph deprecated : Deprecated {
    "POST /v1/charges"
}

require no_calls_to_deprecated_operations {
    severity: warning
    when: calls(c, op, f, l), Deprecated(op)
    message: "{c} calls {op}, which is deprecated ({f}:{l})"
}`;
const verification = `for retriable in [${story.behavior.cases.join(', ')}] {\n  let trace = need ${story.behavior.checker}(\n    handler: orders.place_order,\n    through: link,\n    retriable: retriable\n  )\n  expect trace.response.status == 502\n  ${story.behavior.expect}\n    because "clients retry only when billing says they may"\n}`;
for (const [marker, source, filename, caption, label, kind] of [
  ['IMPACT_CONFIG', finding, 'lemma check --change billing', 'From the quickstart.', 'example finding'],
  ['REQUIREMENT_CONFIG', requirement, 'mod.lm', 'From the requirements tutorial.', 'Lemma requirement example', 'config'],
  ['VERIFICATION_CONFIG', verification, 'orders/mod.lm', 'Behavior excerpt · both cases, asserted against the checker’s evidence.', 'behavior replay example'],
]) html = html.replace(`<!-- ${marker} -->`, renderSnippet(source, filename, caption, label, kind));
html = html.replace('<!-- SETUP_CONFIG -->', renderFileTabs(setupFiles, 'Everything that finding needed.', 'Setup files'));
for (const [marker, file] of [['HERO_ART','hero.svg'],['WORKFLOW_ART','workflow.svg'],['VERIFY_ART','verify.svg']]) {
  html = html.replace(`<!-- ${marker} -->`, readFileSync(path.join(root, 'src', file), 'utf8'));
}
writeFileSync(path.join(output, 'index.html'), html);
cpSync(path.join(root, 'assets'), path.join(output, 'assets'), {recursive: true});
// TODO: bring the design system up to date, then track it and link its page from the site again.
cpSync(path.join(root, 'design-system'), path.join(output, 'design-system'), {recursive: true});
buildDocs({root,output});
console.log(`Built static site: ${output}`);
