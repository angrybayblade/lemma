const root = document.documentElement;
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
const motionButton = document.querySelector('.motion-toggle');
let manuallyPaused = false;
function updateMotion() {
  const paused = manuallyPaused || reducedMotion.matches;
  root.dataset.motion = paused ? 'paused' : 'on';
  motionButton.setAttribute('aria-pressed', String(paused));
  const label = reducedMotion.matches ? 'Motion off' : paused ? 'Play motion' : 'Pause motion';
  motionButton.querySelector('[data-motion-label]').textContent = label;
  motionButton.setAttribute('aria-label', label);
  motionButton.querySelector('.motion-icon path').setAttribute('d', paused ? 'M8 5l11 7-11 7z' : 'M8 5v14M16 5v14');
  motionButton.setAttribute('aria-disabled', String(reducedMotion.matches));
  if (paused) document.querySelectorAll('[data-reveal]').forEach(element => element.classList.add('is-visible'));
}
motionButton.addEventListener('click', () => { if (reducedMotion.matches) return; manuallyPaused = !manuallyPaused; updateMotion(); });
reducedMotion.addEventListener('change', updateMotion);
updateMotion();
const revealObserver = new IntersectionObserver(entries => {
  entries.forEach(entry => { if (entry.isIntersecting) { entry.target.classList.add('is-visible'); revealObserver.unobserve(entry.target); } });
}, {threshold:0.12});
document.querySelectorAll('[data-reveal]').forEach(element => revealObserver.observe(element));
const story = document.querySelector('[data-diagram="story"]');
const chapters = [...document.querySelectorAll('[data-scene-step]')];
const sceneAnchors = [...document.querySelectorAll('[data-scene-anchor]')];
const sceneCaptions = {hero:'The agent development loop.'};
document.querySelectorAll('[data-chapter-scene]').forEach(slot => { sceneCaptions[slot.dataset.chapterScene] = slot.dataset.caption; });
const mobileStory = window.matchMedia('(max-width: 850px)');
const replay = document.querySelector('.story-replay');
function selectScene(scene) {
  story.dataset.scene = scene;
  replay.setAttribute('aria-hidden', String(scene !== 'verification'));
  replay.inert = scene !== 'verification';
  if (['impact','setup','model'].includes(scene)) {
    story.dataset.phase = scene;
    story.querySelector('[data-layer="workflow"]').dataset.phase = scene;
  }
  chapters.forEach(chapter => chapter.classList.toggle('is-active', chapter.dataset.sceneStep === scene));
  document.getElementById('story-caption').textContent = sceneCaptions[scene];
}
let storyFrame = 0;
function sceneAtReadingLine(midpoint) {
  const positions = sceneAnchors.map(anchor => ({scene:anchor.dataset.sceneAnchor,top:anchor.getBoundingClientRect().top}));
  let candidate = 0;
  positions.forEach((position,index) => { if (position.top <= midpoint) candidate = index; });
  const current = positions.findIndex(position => position.scene === story.dataset.scene);
  // A small deadband keeps the handoff steady when scrolling around a heading.
  if (candidate > current && positions[candidate].top > midpoint - 20) return positions[current].scene;
  if (candidate < current && positions[current].top < midpoint + 20) return positions[current].scene;
  return positions[candidate].scene;
}
function updateStory() {
  storyFrame = 0;
  // Phones keep the overview here: each chapter carries its own diagram.
  selectScene(mobileStory.matches ? 'hero' : sceneAtReadingLine(window.innerHeight / 2));
  updateNavigation();
}
function scheduleStory() { if (!storyFrame) storyFrame = requestAnimationFrame(updateStory); }
function positionStory() {
  story.style.setProperty('--stage-offset', `${Math.max(24,(window.innerHeight-story.offsetHeight)/2)}px`);
  scheduleStory();
}
window.addEventListener('scroll', scheduleStory, {passive:true});
window.addEventListener('resize', positionStory);
mobileStory.addEventListener('change', positionStory);
new ResizeObserver(positionStory).observe(story);
const dock = document.querySelector('.floating-dock');
dock.querySelectorAll('.dock-item').forEach(item => {
  item.addEventListener('pointerenter', () => dock.removeAttribute('data-tooltip-dismissed'));
  item.addEventListener('focus', () => dock.removeAttribute('data-tooltip-dismissed'));
  item.addEventListener('click', () => dock.setAttribute('data-tooltip-dismissed', ''));
});
document.addEventListener('keydown', event => { if (event.key === 'Escape') dock.setAttribute('data-tooltip-dismissed', ''); });
const sectionLinks = [...dock.querySelectorAll('a[href^="#"]')];
function updateNavigation() {
  const atSetup = document.querySelector('#start').getBoundingClientRect().top < window.innerHeight * .55;
  const isPastHero = mobileStory.matches
    ? document.querySelector('#workflow').getBoundingClientRect().top < window.innerHeight * .55
    : story.dataset.scene !== 'hero';
  const hash = atSetup ? '#start' : isPastHero ? '#workflow' : '#top';
  sectionLinks.forEach(link => {
    if (link.hash === hash) link.setAttribute('aria-current', 'location');
    else link.removeAttribute('aria-current');
  });
}
// On phones the dock floats over the text being read, so it tucks away while
// reading onward and returns on the way back, or at the end of the page.
const compactDock = window.matchMedia('(max-width: 700px)');
let lastScroll = window.scrollY;
function updateDock() {
  // Checked first: the last step into the end of the page can be a small one.
  if (window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 4) {
    dock.removeAttribute('data-tucked');
    return;
  }
  const distance = window.scrollY - lastScroll;
  if (Math.abs(distance) < 8) return;
  lastScroll = window.scrollY;
  const isReadingOn = distance > 0 && window.scrollY > 80;
  dock.toggleAttribute('data-tucked', compactDock.matches && isReadingOn && !dock.contains(document.activeElement));
}
window.addEventListener('scroll', updateDock, {passive:true});
compactDock.addEventListener('change', () => dock.removeAttribute('data-tucked'));
dock.addEventListener('focusin', () => dock.removeAttribute('data-tucked'));
positionStory();
const copyStatus = document.getElementById('copy-status');
let statusTimeout;
document.querySelectorAll('[data-copy]').forEach(button => button.addEventListener('click', async () => {
  const isConfig = button.dataset.copyKind === 'config';
  const noun = isConfig ? 'Config' : button.dataset.copyKind === 'snippet' ? 'Code' : 'Command';
  try { await navigator.clipboard.writeText(button.dataset.copy); copyStatus.textContent = `${noun} copied.`; }
  catch { copyStatus.textContent = `Select the ${noun.toLowerCase()} text to copy it.`; }
  clearTimeout(statusTimeout); statusTimeout = setTimeout(() => { copyStatus.textContent = ''; }, 4000);
}));
document.querySelectorAll('[data-file-tabs]').forEach(figure => {
  const tabs = [...figure.querySelectorAll('[role="tab"]')];
  const copyButton = figure.querySelector('[data-copy]');
  function selectTab(selected) {
    tabs.forEach(tab => {
      const isSelected = tab === selected;
      tab.setAttribute('aria-selected', String(isSelected));
      tab.tabIndex = isSelected ? 0 : -1;
      const panel = document.getElementById(tab.getAttribute('aria-controls'));
      panel.hidden = !isSelected;
      if (isSelected) copyButton.dataset.copy = panel.textContent;
    });
  }
  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => selectTab(tab));
    tab.addEventListener('keydown', event => {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
      const step = event.key === 'ArrowRight' ? 1 : -1;
      const next = tabs[(index + step + tabs.length) % tabs.length];
      selectTab(next);
      next.focus();
    });
  });
});
// Delegated, because phones add a copy of this switch to the behavior chapter.
document.addEventListener('click', event => {
  const button = event.target.closest('[data-example]');
  if (!button) return;
  const state = button.dataset.example;
  const before = state === 'before';
  document.querySelectorAll('[data-layer="verification"]').forEach(layer => { layer.dataset.state = state; });
  document.querySelectorAll('.story-figure').forEach(figure => { figure.dataset.state = state; });
  if (!mobileStory.matches) selectScene('verification');
  updateNavigation();
  document.querySelectorAll('[data-example]').forEach(control => control.setAttribute('aria-pressed', String(control.dataset.example === state)));
  document.querySelectorAll('[data-replay-value]').forEach(label => { label.textContent = before ? 'missing' : label.dataset.replayValue; });
  const status = before ? 'Before fix: the retry flag is lost in both cases.' : 'After fix: the retry flag reaches the caller in both cases.';
  document.querySelectorAll('.story-replay [role="status"]').forEach(element => { element.textContent = status; });
});

