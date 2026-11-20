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
for (const file of ['styles.css', 'app.js', 'diagrams-webgl.js', 'diagram-kit.js', 'diagram-scenes.js', 'change-story.js']) cpSync(path.join(root, 'src', file), path.join(output, file));
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
// The stop gate's message for the tutorial's failing cases (docs/tutorials/4-behaviors.md), each finding shortened.
const gate = [
  '`lemma check --change` found errors your edits introduced in orders:',
  '- [orders::passes_on_retriable] `retriable` is missing; retriable = false',
  '- [orders::passes_on_retriable] `retriable` is missing; retriable = true',
  'Fix them, or say why each is expected, before finishing.',
].join('\n');
for (const [marker, source, filename, caption, label, kind] of [
  ['IMPACT_CONFIG', finding, 'lemma check --change billing', 'From the quickstart.', 'example finding'],
  ['REQUIREMENT_CONFIG', requirement, 'mod.lm', 'From the requirements tutorial.', 'Lemma requirement example', 'config'],
  ['GATE_CONFIG', gate, 'stop gate', 'What the agent hears when it tries to finish, shortened.', 'stop gate message'],
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
// What the hero's button copies, for a person to paste into their coding agent. It describes Lemma
// by building it up: each step has the agent do something on the person's own code, then says what
// that shows, one step at a time. Step 4 hands off to the lemma-onboarding skill, so keep the install
// step and the instruction line in line with skills/lemma-onboarding/SKILL.md.
const agentPrompt = `Set up Lemma (https://lemma.systems) in this codebase and walk me through it on our own code. Do one step, show me the result and what it shows, then wait for me to say go on. Lemma is pre-release, so expect some misses: https://lemma.systems/docs/pre-release/ lists what it does not do yet.

For the wider picture, read https://lemma.systems/llms.txt first: every docs page as Markdown, with a line on what each is for. Open the pages a step needs, or https://lemma.systems/llms-full.txt for all of them.

Ask me three times: before installing anything, to confirm which repositories make up the system, and before writing to them (a \`mod.lm\`, the throwaway edit, the stop gate or the agent instructions). Do not commit or push.

1. Install. Run:

   curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | LEMMA_SKILLS=1 sh

   Make sure ~/.local/bin is on PATH. If you load skills from somewhere other than ~/.claude/skills, set LEMMA_SKILLS_DIR. Then run \`lemma providers\`, install the language servers this codebase needs, and run \`lemma index\` in the repository: lookups need its index. Add \`lemma mcp\` as an MCP server for yourself; its tools may only load from your next session, so use the \`lemma\` command in this one. A background daemon runs those servers and watches the code: what an IDE knows, without an editor open.

2. Find code. Pick a function with a common name and put Lemma's references next to grep's. Lemma's come from the language server, so they skip other symbols with the same name and mentions in comments and strings, and follow imports and aliases. Point out what grep got wrong.

3. Time it. Run \`time lemma graph <function>\` twice on a central function and show me its callers and callees, each with file:line, and both measured times. The first run may start the servers; the second is the warm path, fast enough to use on every step.

4. Catch a break. Describe the system with the lemma-onboarding skill: a short \`mod.lm\` per repository saying what it offers and what it calls. Then make a throwaway edit that would break a caller, run \`lemma check --change <repository>\`, show me the report, and undo the edit. With several repositories, break a caller in another one; with one, break a caller across an HTTP call or a generated client. Either way, the compiler cannot see that break. The check reads only what the change reaches, so it takes seconds and fits in the loop.

5. Gate the work (experimental). Run \`lemma check --change <repository>\` as if you were finishing: the stop gate, once a project turns it on, runs this when an agent tries to finish, and sends it back if its change introduced errors. Behaviors (beta) add checks that run the real code. If you are Claude Code, turn the gate on with the lemma-verify skill (it is a Claude Code Stop hook); it may only apply from the next session.

6. Sum up, in two or three sentences, what this means for me on long tasks, and for a team whose services and repositories depend on each other. Then add this line to the instructions file this project already uses (CLAUDE.md, AGENTS.md or similar; create one only if there is none): "Lemma is your IDE here: use it for symbols, references, callers and implementations, and text search for strings and configuration. Run \`lemma check --change <repository>\` before calling a change done."`;
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
