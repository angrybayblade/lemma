/* Local guide interactions. No analytics, remote services, or persistence. */
const root = document.documentElement;
const themeButton = document.getElementById('theme-toggle');
themeButton.addEventListener('click', () => {
  const isDark = root.dataset.lmTheme !== 'dark';
  root.dataset.lmTheme = isDark ? 'dark' : 'light';
  themeButton.setAttribute('aria-pressed', String(isDark));
  themeButton.firstChild.textContent = isDark ? 'Light preview ' : 'Dark preview ';
});

const copyStatus = document.getElementById('copy-status');
let statusTimer;
async function copyValue(value) {
  try {
    await navigator.clipboard.writeText(value);
    copyStatus.textContent = `Copied ${value}`;
  } catch {
    copyStatus.textContent = `Copy this value: ${value}`;
  }
  window.clearTimeout(statusTimer);
  statusTimer = window.setTimeout(() => { copyStatus.textContent = ''; }, 4500);
}
document.querySelectorAll('[data-color]').forEach(button => {
  button.addEventListener('click', () => copyValue(button.dataset.color));
});

document.getElementById('show-disabled').addEventListener('click', event => {
  const example = document.getElementById('disabled-example');
  example.hidden = !example.hidden;
  event.currentTarget.textContent = example.hidden ? 'Show disabled' : 'Hide disabled';
});

const labelField = document.getElementById('repo-label');
const labelError = document.getElementById('label-error');
const labelSuccess = document.getElementById('label-success');
document.getElementById('label-form').addEventListener('submit', event => {
  event.preventDefault();
  const isValid = /^[a-z][a-z0-9_-]*$/.test(labelField.value);
  labelField.setAttribute('aria-invalid', String(!isValid));
  labelError.hidden = isValid;
  labelError.textContent = isValid ? '' : 'Start with a lowercase letter. Use only lowercase letters, numbers, dashes, or underscores.';
  labelSuccess.textContent = isValid ? 'Valid example label.' : '';
  if (!isValid) labelField.focus();
});
labelField.addEventListener('input', () => {
  labelField.removeAttribute('aria-invalid');
  labelError.hidden = true;
  labelError.textContent = '';
  labelSuccess.textContent = '';
});

const navLinks = [...document.querySelectorAll('.sidebar nav a')];
const sections = [...document.querySelectorAll('main > section')];
function updateNavigation() {
  let current = sections[0];
  sections.forEach(section => {
    if (section.getBoundingClientRect().top <= Math.max(100, innerHeight * .2)) current = section;
  });
  navLinks.forEach(link => {
    if (link.hash === `#${current.id}`) link.setAttribute('aria-current', 'location');
    else link.removeAttribute('aria-current');
  });
}
let navigationFrame;
function scheduleNavigation() {
  cancelAnimationFrame(navigationFrame);
  navigationFrame = requestAnimationFrame(updateNavigation);
}
window.addEventListener('scroll', scheduleNavigation, {passive: true});
window.addEventListener('resize', scheduleNavigation, {passive: true});
window.addEventListener('hashchange', scheduleNavigation);
scheduleNavigation();
