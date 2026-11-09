// Highlights `.lm` source for the static site: the same reading of the
// language as editors/vscode/syntaxes/lemma.tmLanguage.json, as HTML spans.
const TOKEN = /\/\/[^\n]*|\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\\n])*"|`[^`\n]+`|@(?:base|head)\b|->|:-|==|!=|<=|>=|&&|\|\||\b\d+\b|[A-Za-z_][A-Za-z0-9_]*/g;
// Reserved everywhere, as the lexer's RESERVED list says.
const KEYWORDS = new Set(['module', 'import', 'export', 'from', 'as', 'let', 'fn', 'if', 'else', 'for', 'in', 'return', 'expect', 'because', 'need']);
const CONSTANTS = new Set(['true', 'false', 'null', 'self']);
// Keywords only where a declaration starts: `check` may also be a field.
const DECLARATIONS = new Set(['workspace', 'repo', 'service', 'entity', 'type', 'client', 'operation', 'graph', 'rule', 'require', 'check', 'allow', 'behavior', 'checker', 'relation', 'run', 'skip']);
// What the word after these is: the name being declared.
const NAMES_NEXT = new Set(['module', 'as', 'let', 'fn', 'repo', 'service', 'entity', 'type', 'operation', 'graph', 'rule', 'require', 'check', 'allow', 'behavior', 'checker', 'relation', 'run', 'skip']);

const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
const span = (kind, text) => `<span class="syntax-${kind}">${escape(text)}</span>`;

function classifyWord(word, source, start, end, declaring) {
  const before = source.slice(source.lastIndexOf('\n', start) + 1, start);
  const after = source.slice(end);
  const isMember = /\.\s*$/.test(before);
  if (!isMember && KEYWORDS.has(word)) return 'keyword';
  if (!isMember && CONSTANTS.has(word)) return 'constant';
  const startsDeclaration = /^\s*(?:(?:export|client)\s+)*$/.test(before);
  if (startsDeclaration && DECLARATIONS.has(word) && !/^\s*[:=.(]/.test(after)) return 'keyword';
  if (declaring) return 'name';
  if (/^\s*:(?![-:])/.test(after) && !isMember) return 'property';
  if (/^[A-Z]/.test(word)) return 'type';
  if (/^\s*\(/.test(after)) return 'function';
  return '';
}

export function highlightLm(source) {
  let html = '';
  let offset = 0;
  let declaring = false;
  for (const match of source.matchAll(TOKEN)) {
    const value = match[0];
    const end = match.index + value.length;
    html += escape(source.slice(offset, match.index));
    offset = end;
    if (value.startsWith('//') || value.startsWith('/*')) html += span('comment', value);
    else if (value.startsWith('"')) html += span('string', value);
    else if (/^\d/.test(value)) html += span('number', value);
    else if (value.startsWith('@')) html += span('keyword', value);
    else if (!/^[A-Za-z_`]/.test(value)) html += span('operator', value);
    else {
      const kind = classifyWord(value, source, match.index, end, declaring);
      html += kind ? span(kind, value) : escape(value);
      // A dotted name, such as `lemma.http.*`, is one name.
      declaring = (kind === 'keyword' && NAMES_NEXT.has(value)) || (kind === 'name' && source[end] === '.');
    }
  }
  return markBlankLines(html + escape(source.slice(offset)));
}

// Blank lines get their own element so CSS can shorten them. Their newlines
// stay inside it, so copied or selected text still matches the source.
function markBlankLines(html) {
  return html.replace(/\n(\n+)/g, (_, blank) => `\n<span class="code-blank">${blank}</span>`);
}
