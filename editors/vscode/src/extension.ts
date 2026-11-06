// VS Code client for Lemma's `.lm` files. Highlighting comes from the
// TextMate grammar in syntaxes/; everything else comes from the language
// server that `lm lsp` runs over stdio.

import * as vscode from 'vscode'
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from 'vscode-languageclient/node'

const configurationSection = 'lemma'
const defaultBinary = 'lm'

let client: LanguageClient | undefined

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand('lemma.restartServer', restartServer),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration(`${configurationSection}.lmPath`)) {
        void restartServer()
      }
    }),
  )
  await startServer()
}

export async function deactivate(): Promise<void> {
  await stopServer()
}

/** The binary to run, from the `lemma.lmPath` setting. */
function configuredBinary(): string {
  const configured = vscode.workspace.getConfiguration(configurationSection).get<string>('lmPath')
  return configured?.trim() || defaultBinary
}

function serverOptions(binary: string): ServerOptions {
  return { command: binary, args: ['lsp'] }
}

function clientOptions(): LanguageClientOptions {
  return {
    documentSelector: [
      { scheme: 'file', language: 'lemma' },
      { scheme: 'untitled', language: 'lemma' },
    ],
  }
}

async function startServer(): Promise<void> {
  const binary = configuredBinary()
  const started = new LanguageClient('lemma', 'Lemma', serverOptions(binary), clientOptions())
  try {
    await started.start()
  } catch (error) {
    // Highlighting still works without the server, so report the failure
    // and carry on rather than failing activation.
    const reason = error instanceof Error ? error.message : String(error)
    const openSettings = 'Open Settings'
    const choice = await vscode.window.showErrorMessage(
      `Lemma: could not start \`${binary} lsp\` (${reason}). Set lemma.lmPath to the lm binary.`,
      openSettings,
    )
    if (choice === openSettings) {
      await vscode.commands.executeCommand(
        'workbench.action.openSettings',
        `${configurationSection}.lmPath`,
      )
    }
    return
  }
  client = started
}

async function stopServer(): Promise<void> {
  const running = client
  client = undefined
  if (running === undefined) {
    return
  }
  await running.stop()
}

async function restartServer(): Promise<void> {
  await stopServer()
  await startServer()
}
