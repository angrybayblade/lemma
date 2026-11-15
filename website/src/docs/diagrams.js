// The landing page's diagrams, in the docs. A page marks where one goes with
// `<!-- diagram: <scene> -->`; the build puts a slot there and carries the
// landing's figures as templates. This mounts a copy in each slot, and gives
// the docs what the landing's own script gives it: motion a reader can pause,
// and the behavior figure's switch between the change and the fix.
import {createStoryEntries, createTaskLoopMesh, initStory, mountChapterDiagram, TASK_SIZE} from '/diagrams-webgl.js';

const root = document.documentElement;
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
const PAUSED_KEY = 'lemma-docs-motion-paused';
const PAUSE_PATH = 'M8 5v14M16 5v14';
const PLAY_PATH = 'M8 5l11 7-11 7z';

// The reader's choice is a convenience: without storage, motion just plays.
function readPaused() {
  try { return window.localStorage.getItem(PAUSED_KEY) === 'true'; } catch { return false; }
}
function writePaused(paused) {
  try { window.localStorage.setItem(PAUSED_KEY, String(paused)); } catch { /* not kept */ }
}

let paused = readPaused();
const motionButtons = [];

function updateMotion() {
  root.dataset.motion = reducedMotion.matches ? 'off' : paused ? 'paused' : 'on';
  const label = reducedMotion.matches ? 'Motion off' : paused ? 'Play motion' : 'Pause motion';
  for (const button of motionButtons) {
    button.setAttribute('aria-label', label);
    button.setAttribute('aria-pressed', String(paused));
    button.setAttribute('aria-disabled', String(reducedMotion.matches));
    button.querySelector('path').setAttribute('d', paused ? PLAY_PATH : PAUSE_PATH);
  }
}

// One control per diagram; any of them pauses every diagram on the page.
function addMotionButton(slot) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'diagram-motion';
  button.innerHTML = `<svg class="lm-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="${PAUSE_PATH}"/></svg>`;
  button.addEventListener('click', () => {
    if (reducedMotion.matches) return;
    paused = !paused;
    writePaused(paused);
    updateMotion();
  });
  slot.append(button);
  motionButtons.push(button);
}

function mountTaskLoop(slot, template) {
  const figure = template.content.querySelector('[data-diagram="task"]').cloneNode(true);
  figure.setAttribute('role', 'img');
  figure.setAttribute('aria-label', slot.dataset.caption);
  slot.replaceChildren(figure);
  initStory(figure, {task: {mesh: createTaskLoopMesh(), mode: 0, size: TASK_SIZE, layer: 'task'}});
}

// The behavior figure's switch, as the landing's script runs it, kept to the
// figure it belongs to.
function selectReplay(button) {
  const figure = button.closest('.story-figure');
  const state = button.dataset.example;
  const before = state === 'before';
  figure.dataset.state = state;
  figure.querySelectorAll('[data-layer="verification"]').forEach(layer => { layer.dataset.state = state; });
  figure.querySelectorAll('[data-example]').forEach(control => control.setAttribute('aria-pressed', String(control.dataset.example === state)));
  figure.querySelectorAll('[data-replay-value]').forEach(label => { label.textContent = before ? 'missing' : label.dataset.replayValue; });
  const status = before ? 'With the change: the retry flag is lost in both cases.' : 'With the fix: the retry flag reaches the caller in both cases.';
  figure.querySelectorAll('.story-replay [role="status"]').forEach(element => { element.textContent = status; });
}

const storyTemplate = document.getElementById('docs-story-figure');
const taskTemplate = document.getElementById('docs-task-figure');
const chapterSlots = [...document.querySelectorAll('[data-chapter-scene]')];
const taskSlots = [...document.querySelectorAll('[data-docs-task]')];
updateMotion();
reducedMotion.addEventListener('change', updateMotion);
if (storyTemplate && chapterSlots.length) {
  const source = storyTemplate.content.querySelector('[data-diagram="story"]');
  const entries = createStoryEntries();
  for (const slot of chapterSlots) {
    mountChapterDiagram(slot, source, entries);
    addMotionButton(slot);
  }
}
if (taskTemplate) {
  for (const slot of taskSlots) {
    mountTaskLoop(slot, taskTemplate);
    addMotionButton(slot);
  }
}
updateMotion();
document.addEventListener('click', event => {
  const button = event.target.closest('.chapter-diagram [data-example]');
  if (button) selectReplay(button);
});
