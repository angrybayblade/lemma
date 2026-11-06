// Checks the TextMate grammar. VS Code silently drops a pattern whose regex
// Oniguruma rejects, so every regex is compiled here. Then the ```lm blocks
// of spec 13 and every `.lm` file in the repository are tokenized, failing on
// any `invalid` scope or on a rule body that runs past its declaration, and
// a few constructs the grammar must tell apart are checked by their scopes.
//
// Usage: node scripts/check-grammar.mjs

import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import oniguruma from 'vscode-oniguruma'
import textmate from 'vscode-textmate'

const extensionDirectory = join(dirname(fileURLToPath(import.meta.url)), '..')
const repository = join(extensionDirectory, '..', '..')
const grammarPath = join(extensionDirectory, 'syntaxes', 'lemma.tmLanguage.json')
const specPath = join(repository, '.specs', '13-lm-grammar.md')

// Each case names a token in `source` and a scope it must carry.
const cases = [
  { source: 'export service Functions {', token: 'service', scope: 'storage.type.service' },
  { source: 'let level = config.service', token: 'service', scope: 'variable.other.property' },
  {
    source: '    client: http.generated_client("x")',
    token: 'client',
    scope: 'variable.other.property',
  },
  { source: '    client operation call {}', token: 'operation', scope: 'storage.type.operation' },
  { source: '    level: check', token: 'check', scope: 'constant.language.check' },
  { source: 'check main {', token: 'main', scope: 'entity.name.function' },
  { source: '    run lemma.links.*', token: 'links', scope: 'entity.name.namespace' },
  {
    source: 'let tools = python.module("x")',
    token: 'module',
    scope: 'entity.name.function.member',
  },
  { source: '    (a, b) :- reachable(a, b)', token: 'a', scope: 'variable.parameter' },
  {
    source: '    (a, b) :- reachable(a, b)',
    token: 'reachable',
    scope: 'entity.name.function.predicate',
  },
  {
    source: '    (a, c) :-\n        reachable(a, b),\n        service_dependencies(b, c)',
    token: 'c',
    occurrence: 2,
    scope: 'variable.parameter',
  },
  {
    source: '    (target) :- operation@base(operation: target), !Link(Worker, target)',
    token: '@base',
    scope: 'storage.modifier.snapshot',
  },
  {
    source: '    (target) :- operation@base(operation: target), !Link(Worker, target)',
    token: 'Worker',
    scope: 'entity.name.type',
  },
  { source: '    when: consumer.file == "x"', token: 'consumer', scope: 'variable.parameter' },
  { source: '    message: "{consumer} calls"', token: 'consumer', scope: 'meta.interpolation' },
  {
    source: '    expect a == b\n        because "why"',
    token: 'because',
    scope: 'keyword.control',
  },
  { source: 'graph links : Link {', token: 'links', scope: 'entity.name.function' },
  { source: '    Worker.call -> Functions.setup', token: '->', scope: 'keyword.operator.arrow' },
  {
    source: 'rule reachable(a: Text, b: Text) {',
    token: 'reachable',
    scope: 'entity.name.function',
  },
  { source: 'rule reachable(a: Text, b: Text) {', token: 'b', scope: 'variable.parameter' },
  { source: '    }\n    else {', token: 'else', scope: 'keyword.control' },
  { source: '    severity: info', token: 'info', scope: 'constant.language.info' },
  {
    source: 'let paths = { include: ["src"], exclude: [] }',
    token: 'exclude',
    scope: 'variable.other.property',
  },
]

async function loadOniguruma() {
  const require = createRequire(import.meta.url)
  const wasm = readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'))
  await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength))
}

async function loadGrammar() {
  const registry = new textmate.Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns),
      createOnigString: (text) => new oniguruma.OnigString(text),
    }),
    loadGrammar: async (scopeName) => {
      if (scopeName !== 'source.lemma') return null
      return textmate.parseRawGrammar(readFileSync(grammarPath, 'utf8'), grammarPath)
    },
  })
  return registry.loadGrammar('source.lemma')
}

/** Every regex in the grammar, with a path saying where it is. */
function collectRegexes(node, path, found) {
  if (Array.isArray(node)) {
    node.forEach((child, index) => collectRegexes(child, `${path}[${index}]`, found))
    return found
  }
  if (node === null || typeof node !== 'object') return found
  for (const [key, value] of Object.entries(node)) {
    if (['match', 'begin', 'end', 'while'].includes(key) && typeof value === 'string') {
      found.push({ path: `${path}.${key}`, regex: value })
    } else {
      collectRegexes(value, `${path}.${key}`, found)
    }
  }
  return found
}

function tokenize(grammar, source) {
  const tokens = []
  let state = textmate.INITIAL
  for (const line of source.split('\n')) {
    const result = grammar.tokenizeLine(line, state)
    for (const token of result.tokens) {
      tokens.push({ text: line.slice(token.startIndex, token.endIndex), scopes: token.scopes })
    }
    state = result.ruleStack
  }
  return tokens
}

/** The texts to tokenize, each with a name for messages. */
function sources() {
  const spec = readFileSync(specPath, 'utf8')
  const blocks = [...spec.matchAll(/^```lm\n([\s\S]*?)^```/gm)].map((match, index) => ({
    name: `spec 13 block ${index + 1}`,
    text: match[1],
  }))
  const listed = execFileSync(
    'git',
    [
      '-C',
      repository,
      'ls-files',
      '-z',
      '--cached',
      '--others',
      '--exclude-standard',
      '--',
      '*.lm',
    ],
    { encoding: 'utf8' },
  )
  const files = listed
    .split('\0')
    .filter((path) => path !== '')
    .map((path) => ({ name: path, text: readFileSync(join(repository, path), 'utf8') }))
  return [...blocks, ...files]
}

/** Problems in one tokenized text: invalid tokens, and rule bodies that leak. */
function problems(name, source) {
  const found = []
  let state = textmate.INITIAL
  source.split('\n').forEach((line, index) => {
    const result = grammar.tokenizeLine(line, state)
    state = result.ruleStack
    for (const token of result.tokens) {
      const text = line.slice(token.startIndex, token.endIndex)
      if (token.scopes.some((scope) => scope.startsWith('invalid'))) {
        found.push(`${name}:${index + 1}: \`${text}\` is ${token.scopes.join(' ')}`)
      }
    }
    // A rule clause or `when:` body ends before its declaration's next field
    // or closing brace; a scope reaching those means a body never closed.
    const trimmed = line.trim()
    const leaks = trimmed === '}' || /^(?!when\b)[a-z_]+:(?!-)/.test(trimmed)
    const first = result.tokens.find((token) => line.slice(token.startIndex, token.endIndex).trim())
    if (leaks && first && first.scopes.some((scope) => scope.startsWith('meta.rule'))) {
      found.push(`${name}:${index + 1}: a rule body runs into \`${trimmed}\``)
    }
  })
  return found
}

const failures = []

await loadOniguruma()

for (const { path, regex } of collectRegexes(
  JSON.parse(readFileSync(grammarPath, 'utf8')),
  '$',
  [],
)) {
  try {
    new oniguruma.OnigScanner([regex])
  } catch (error) {
    failures.push(`${path}: ${regex}: ${error.message}`)
  }
}

const grammar = await loadGrammar()

const texts = sources()
for (const { name, text } of texts) {
  failures.push(...problems(name, text))
}

for (const { source, token, occurrence = 1, scope } of cases) {
  const matches = tokenize(grammar, source).filter((candidate) => candidate.text === token)
  const found = matches[occurrence - 1]
  if (found === undefined) {
    failures.push(`${JSON.stringify(source)}: no token \`${token}\` (occurrence ${occurrence})`)
  } else if (!found.scopes.some((candidate) => candidate.startsWith(scope))) {
    failures.push(
      `${JSON.stringify(source)}: \`${token}\` is ${found.scopes.join(' ')}, not ${scope}`,
    )
  }
}

if (failures.length > 0) {
  console.error(failures.join('\n'))
  process.exit(1)
}
console.log(`TextMate grammar: ${texts.length} sources and ${cases.length} cases tokenized`)
