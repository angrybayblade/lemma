// Runs inside a real VS Code, with the extension installed from its .vsix
// and the workspace opened on editors/fixture. The fixture module is valid
// except for one requirement naming a predicate nothing declares, so the
// language server has exactly one problem to report.

import * as assert from 'node:assert/strict'
import * as vscode from 'vscode'

const extensionId = 'lemma.lemma-lm'
const waitTimeoutMs = 30_000
const pollIntervalMs = 100

/** One token as VS Code's `_workbench.captureSyntaxTokens` reports it. */
interface CapturedToken {
  /** The token's text. */
  c: string
  /** Its TextMate scopes, outermost first, separated by spaces. */
  t: string
}

/** A definition target, whichever form the provider answered with. */
interface Target {
  uri: vscode.Uri
  range: vscode.Range
}

function fixtureUri(): vscode.Uri {
  const folder = vscode.workspace.workspaceFolders?.[0]
  assert.ok(folder, 'VS Code opened no workspace folder; the runner passes editors/fixture')
  return vscode.Uri.joinPath(folder.uri, 'mod.lm')
}

/** The position of the `occurrence`-th `text` in `document`. */
function positionOf(document: vscode.TextDocument, text: string, occurrence = 1): vscode.Position {
  let offset = -1
  for (let found = 0; found < occurrence; found++) {
    offset = document.getText().indexOf(text, offset + 1)
    assert.notEqual(offset, -1, `the fixture has no occurrence ${occurrence} of \`${text}\``)
  }
  return document.positionAt(offset)
}

/** Polls `probe` until it returns a value, failing after a timeout. */
async function waitFor<T>(what: string, probe: () => T | undefined): Promise<T> {
  const deadline = Date.now() + waitTimeoutMs
  for (;;) {
    const value = probe()
    if (value !== undefined) {
      return value
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out after ${waitTimeoutMs} ms waiting for ${what}`)
    }
    await new Promise((resolve) => setTimeout(resolve, pollIntervalMs))
  }
}

function lemmaDiagnostics(uri: vscode.Uri): vscode.Diagnostic[] | undefined {
  const published = vscode.languages
    .getDiagnostics(uri)
    .filter((diagnostic) => diagnostic.source === 'lemma')
  return published.length > 0 ? published : undefined
}

function hoverText(hovers: vscode.Hover[]): string {
  return hovers
    .flatMap((hover) => hover.contents)
    .map((content) => (typeof content === 'string' ? content : content.value))
    .join('\n')
}

function asTarget(found: vscode.Location | vscode.LocationLink): Target {
  if ('targetUri' in found) {
    return { uri: found.targetUri, range: found.targetSelectionRange ?? found.targetRange }
  }
  return { uri: found.uri, range: found.range }
}

describe('the Lemma extension in VS Code', () => {
  let document: vscode.TextDocument

  before(async () => {
    document = await vscode.workspace.openTextDocument(fixtureUri())
    await vscode.window.showTextDocument(document)
    // The server reads a document when it opens, so its first diagnostics
    // also mean hover and definition have something to answer from.
    await waitFor('diagnostics from `lm lsp`', () => lemmaDiagnostics(document.uri))
  })

  it('runs the extension installed from the packaged .vsix', () => {
    const extension = vscode.extensions.getExtension(extensionId)

    assert.ok(extension, `${extensionId} is not installed`)
    assert.ok(extension.isActive, 'opening a .lm file did not activate the extension')
    const extensionsDirectory = process.env.LEMMA_EXTENSIONS_DIR
    if (extensionsDirectory !== undefined) {
      assert.ok(
        extension.extensionPath.startsWith(extensionsDirectory),
        `the extension runs from ${extension.extensionPath}, not from the installed .vsix in ${extensionsDirectory}`,
      )
    }
  })

  it('opens .lm files in the lemma language', () => {
    assert.equal(document.languageId, 'lemma')
  })

  it('shows the problem the language server reports', () => {
    const diagnostics = lemmaDiagnostics(document.uri) ?? []

    assert.equal(diagnostics.length, 1, JSON.stringify(diagnostics))
    const [problem] = diagnostics
    assert.match(problem.message, /no relation, rule or graph `missing`/)
    assert.equal(problem.severity, vscode.DiagnosticSeverity.Error)
    assert.deepEqual(problem.range.start, positionOf(document, 'missing'))
  })

  it('hovers a relation to show its columns', async () => {
    const use = positionOf(document, 'Owns(_, service)')

    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
      'vscode.executeHoverProvider',
      document.uri,
      use,
    )

    assert.match(hoverText(hovers), /relation Owns: \(team: Text, service: Text\)/)
  })

  it('goes from a use of a relation to its declaration', async () => {
    const use = positionOf(document, 'Owns(_, service)')

    const found = await vscode.commands.executeCommand<(vscode.Location | vscode.LocationLink)[]>(
      'vscode.executeDefinitionProvider',
      document.uri,
      use,
    )

    assert.equal(found.length, 1, JSON.stringify(found))
    const target = asTarget(found[0])
    assert.equal(target.uri.toString(), document.uri.toString())
    assert.equal(target.range.start.line, positionOf(document, 'relation Owns').line)
  })

  it('highlights keywords and strings with the TextMate grammar', async () => {
    const tokens = await vscode.commands.executeCommand<CapturedToken[] | undefined>(
      '_workbench.captureSyntaxTokens',
      document.uri,
    )
    assert.ok(tokens, 'this VS Code has no `_workbench.captureSyntaxTokens` command')
    const scopesOf = (text: string): string => {
      const token = tokens.find((candidate) => candidate.c === text)
      assert.ok(token, `no token \`${text}\` in ${JSON.stringify(tokens.map((t) => t.c))}`)
      return token.t
    }

    assert.match(scopesOf('module'), /\bkeyword\.control\.import\.lemma\b/)
    assert.match(scopesOf('relation'), /\bstorage\.type\.relation\.lemma\b/)
    assert.match(scopesOf('require'), /\bstorage\.type\.require\.lemma\b/)
    assert.match(scopesOf(' is owned by '), /\bstring\.quoted\.double\.lemma\b/)
    assert.match(scopesOf('other'), /\bvariable\.parameter\.lemma\b/)
  })
})
