// The docs' diagrams. A page marks where one goes with `<!-- diagram: <scene> -->`,
// and the build renders that scene's words and drawing there
// (`src/diagram-scenes.js`). This draws the scene's matte relief and motion on
// the figure's canvas, with the landing page's renderer, and gives every
// diagram a pause control.
import {initStory} from '/diagrams-webgl.js';
import {SCENES} from '/diagram-scenes.js';

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

function animate(slot) {
  const name = slot.dataset.docsScene;
  const scene = SCENES[name];
  const figure = slot.querySelector('.kit-figure');
  if (!scene || !figure) return;
  const {mesh, mode, size} = scene.build();
  figure.dataset.scene = name;
  initStory(figure, {[name]: {mesh, mode, size, layer: name}});
  addMotionButton(slot);
}

updateMotion();
reducedMotion.addEventListener('change', updateMotion);
document.querySelectorAll('[data-docs-scene]').forEach(animate);
updateMotion();
