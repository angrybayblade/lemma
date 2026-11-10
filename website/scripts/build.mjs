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
// `filename` picks the highlighting; with `showFilename` false the block has no title bar, only its copy button.
function renderSnippet(source, filename, caption, label, kind = 'snippet', {showFilename = true} = {}) {
  const copy = escape(source).replaceAll('\n', '&#10;');
  const figureClass = showFilename ? 'config-example' : 'config-example config-example--bare';
  const title = showFilename ? `<span>${escape(filename)}</span>` : '';
  return `<figure class="${figureClass}" aria-label="${escape(label)}"><div class="config-heading">${title}<button type="button" data-copy-kind="${kind}" data-copy="${copy}" aria-label="Copy ${escape(label)}"><svg class="lm-icon" aria-hidden="true"><use href="design-system/assets/symbols.svg#copy"/></svg></button></div><pre><code>${filename.endsWith('.lm') ? highlightLm(source) : escape(source)}</code></pre><figcaption>${escape(caption)}</figcaption></figure>`;
}
const COPY_ICON = '<svg class="lm-icon" aria-hidden="true"><use href="design-system/assets/symbols.svg#copy"/></svg>';
// `id` keeps each set of tabs' ids apart; `highlight` renders a panel's source (only `.lm` is highlighted).
function renderFileTabs(files, caption, label, {id = 'file', highlight = highlightLm} = {}) {
  const tabs = files.map(([filename], index) => `<button type="button" role="tab" id="${id}-tab-${index}" aria-controls="${id}-panel-${index}" aria-selected="${index === 0}" tabindex="${index === 0 ? 0 : -1}">${escape(filename)}</button>`).join('');
  const panels = files.map(([, source], index) => `<pre role="tabpanel" id="${id}-panel-${index}" aria-labelledby="${id}-tab-${index}" tabindex="0"${index === 0 ? '' : ' hidden'}><code>${highlight(source)}</code></pre>`).join('');
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
// orders' handler before and after an agent's change. Before, it passes billing's `retriable` on in its
// 502 body. The change moves error bodies into one helper that leaves `retriable` out: every schema
// still matches, and the flag is gone. The fix in docs/tutorials/4-behaviors.md puts it back.
const ordersHandler = {
  before: `def place_order(order):
    result = billing.charge(order["customer"])
    if "error" in result:
        body = {"error": result["error"], "retriable": result["retriable"]}
        return {"status": 502, "body": body}
    return {"status": 201, "body": {"id": result["id"]}}`,
  after: `def error_response(status, message):
    return {"status": status, "body": {"error": message}}

def place_order(order):
    result = billing.charge(order["customer"])
    if "error" in result:
        return error_response(502, result["error"])
    return {"status": 201, "body": {"id": result["id"]}}`,
};
// orders' caller, unchanged by the change. Not from the tutorial: it shows what the dropped flag costs. It offers a retry
// only when `retriable` is true, so without the flag every decline looks final.
const ordersCaller = `export async function placeOrder(order: Order) {
  const response = await fetch(...);
  if (response.status === 502) {
    const { retriable } = await response.json();
    return retriable ? offerRetry(order) : showDeclined(order);
  }
  return showConfirmed(order);
}`;
// The behavior from the same tutorial, shortened.
const verification = `let link = links[orders.charge, api.createCharge]\nfor retriable in [${story.behavior.cases.join(', ')}] {\n  // Ask the checker for this case's evidence; it fakes billing's answer on link.\n  let trace = need ${story.behavior.checker}(\n    handler: orders.place_order,\n    through: link,\n    retriable: retriable\n  )\n  expect trace.response.status == 502\n  ${story.behavior.expect}\n    because "clients retry only when billing says they may"\n}`;
for (const [marker, source, filename, caption, label, kind] of [
  ['IMPACT_CONFIG', finding, 'lemma check --change billing', 'From the quickstart.', 'example finding'],
  ['REQUIREMENT_CONFIG', requirement, 'mod.lm', 'From the requirements tutorial.', 'Lemma requirement example', 'config'],
  ['VERIFICATION_CONFIG', verification, 'orders/mod.lm', 'The behavior: both cases, checked against what orders sent back.', 'behavior replay example'],
]) html = html.replace(`<!-- ${marker} -->`, renderSnippet(source, filename, caption, label, kind));
html = html.replace('<!-- CALLER_CONFIG -->', renderSnippet(ordersCaller, 'checkout.ts', 'The caller offers a retry only when retriable is true. With the breaking change it never arrives, so every decline looks final.', 'caller code', 'snippet', {showFilename: false}));
html = html.replace('<!-- SETUP_CONFIG -->', renderFileTabs(setupFiles, 'Everything Lemma needs.', 'Setup files'));
html = html.replace('<!-- DROPPED_CONFIG -->', renderFileTabs([['Before', ordersHandler.before], ['Breaking change', ordersHandler.after]], 'orders/handlers.py: the change moves error bodies into a helper that leaves out retriable.', 'orders handler, before and with the breaking change', {id: 'handler', highlight: escape}));
// The getting-started steps, one tab per coding agent. Each agent reads skills from its own directory,
// adds MCP servers with its own command and starts a skill its own way; checked against each agent's docs.
const INSTALL_URL = 'https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh';
const AGENTS_SKILLS_DIR = '"$HOME/.agents/skills"';
// The task in step 4 is an example; Lemma mode is the skill that checks a change across repositories as the agent makes it.
const TASK = 'implement support for payment failure retries';
const agents = [
  {id: 'claude', name: 'Claude Code', mcp: 'claude mcp add lemma -- lemma mcp', onboard: '/lemma-onboarding', task: `/lemma-mode ${TASK}`},
  {id: 'codex', name: 'Codex', skillsDir: AGENTS_SKILLS_DIR, mcp: 'codex mcp add lemma -- lemma mcp', onboard: '$lemma-onboarding', task: `$lemma-mode ${TASK}`},
  {id: 'opencode', name: 'OpenCode', skillsDir: AGENTS_SKILLS_DIR, mcp: 'opencode mcp add lemma -- lemma mcp', onboard: 'Use the lemma-onboarding skill to set up Lemma.', task: `Use the lemma-mode skill to ${TASK}.`},
  {id: 'pi', name: 'PI', skillsDir: AGENTS_SKILLS_DIR, mcp: 'pi mcp add lemma -- lemma mcp', onboard: '/skill:lemma-onboarding', task: `/skill:lemma-mode ${TASK}`},
];
// kind is 'shell' for a terminal command or 'prompt' for something typed to the agent.
function renderSetupStep(number, heading, command, label, kind) {
  return `<article><div class="setup-heading"><span>${number} / ${escape(heading)}</span></div><div class="setup-command" data-kind="${kind}"><code title="${escape(command)}">${escape(command)}</code><button type="button" data-copy="${escape(command)}" aria-label="Copy ${escape(label)}">${COPY_ICON}</button></div></article>`;
}
function renderAgentSteps(agent) {
  const skills = agent.skillsDir ? `LEMMA_SKILLS=1 LEMMA_SKILLS_DIR=${agent.skillsDir}` : 'LEMMA_SKILLS=1';
  return renderSetupStep('01', 'Install Lemma and the agent skills', `curl -fsSL ${INSTALL_URL} | ${skills} sh`, 'install command', 'shell')
    + renderSetupStep('02', `Connect ${agent.name} over MCP`, agent.mcp, 'MCP setup command', 'shell')
    + renderSetupStep('03', `Describe your repositories with ${agent.name}`, agent.onboard, 'onboarding prompt', 'prompt')
    + renderSetupStep('04', 'Give it a task', agent.task, 'task prompt', 'prompt');
}
const agentTabs = agents.map((agent, index) => `<button type="button" role="tab" id="agent-tab-${agent.id}" aria-controls="agent-panel-${agent.id}" aria-selected="${index === 0}" tabindex="${index === 0 ? 0 : -1}">${escape(agent.name)}</button>`).join('');
const agentPanels = agents.map((agent, index) => `<div role="tabpanel" id="agent-panel-${agent.id}" aria-labelledby="agent-tab-${agent.id}"${index === 0 ? '' : ' hidden'}>${renderAgentSteps(agent)}</div>`).join('');
html = html.replace('<!-- SETUP_AGENT_TABS -->', `<div class="example-switch setup-agents" role="tablist" aria-label="Coding agent">${agentTabs}</div>`);
html = html.replace('<!-- SETUP_AGENT_PANELS -->', agentPanels);
// What the hero's button copies, for a person to paste into their coding agent. It hands off to the
// lemma-onboarding skill, so keep the install step in line with skills/lemma-onboarding/SKILL.md.
const agentPrompt = `Help me set up Lemma (https://lemma.systems) in my codebase.

Lemma gives coding agents contract checks, cross-repo impact analysis and behavior simulations for connected systems. It reads code through language servers and answers where things are used. Each repository gets a short \`mod.lm\` file naming its services, Lemma infers the calls between them from OpenAPI documents and code, and \`lemma check\` reports what a change breaks in the other repositories. It is pre-release: https://lemma.systems/docs/pre-release/ lists what it does not do yet.

Work in this order:

1. In two or three sentences, tell me what Lemma does and what you are about to install. Wait for my go-ahead.
2. Install the \`lemma\` and \`lm\` binaries into ~/.local/bin and the agent skills into ~/.claude/skills:

   curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | LEMMA_SKILLS=1 sh

   If you load skills from somewhere else, set LEMMA_SKILLS_DIR to that directory. Make sure ~/.local/bin is on PATH.
3. Run \`lemma --version\` and \`lemma providers\`, and tell me what they report.
4. Read the lemma-onboarding skill and offer to start. If I agree, follow it one step at a time.

Ask before anything that changes my machine or repositories: installing language servers, writing a \`mod.lm\`, editing \`.gitignore\` or agent instructions, adding the MCP server, or touching CI. Ask me which repositories make up the system rather than guessing. Do not commit or push unless I say so.`;
html = html.replace('<!-- AGENT_PROMPT -->', escape(agentPrompt).replaceAll('\n', '&#10;'));
for (const [marker, file] of [['HERO_ART','hero.svg'],['WORKFLOW_ART','workflow.svg'],['VERIFY_ART','verify.svg'],['DROPPED_ART','dropped.svg'],['TASK_LOOP_ART','task-loop.svg']]) {
  html = html.replace(`<!-- ${marker} -->`, readFileSync(path.join(root, 'src', file), 'utf8'));
}
writeFileSync(path.join(output, 'index.html'), html);
cpSync(path.join(root, 'assets'), path.join(output, 'assets'), {recursive: true});
// TODO: bring the design system up to date, then track it and link its page from the site again.
cpSync(path.join(root, 'design-system'), path.join(output, 'design-system'), {recursive: true});
buildDocs({root,output});
console.log(`Built static site: ${output}`);
