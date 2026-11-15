// VS Code client for Lemma. Highlighting of `.lm` files comes from the
// TextMate grammar in syntaxes/; everything else about them comes from the
// language server that `lm lsp` runs over stdio.
//
// With `lemma.checkOnChange.enabled`, a second server, `lemma check --lsp`,
// checks each repository of the workspace as its saved files change and
// publishes what the change breaks as diagnostics on any file, in any
// repository. It is experimental and off by default, since it runs checks
// in the background.

import * as vscode from 'vscode'
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from 'vscode-languageclient/node'

const configurationSection = 'lemma'
const defaultLmBinary = 'lm'
const defaultLemmaBinary = 'lemma'

/** The settings that change how a server starts, so changing one restarts them. */
const restartingSettings = [
  'lmPath',
  'lemmaPath',
  'checkOnChange.enabled',
  'checkOnChange.manifest',
].map((name) => `${configurationSection}.${name}`)

let clients: LanguageClient[] = []

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand('lemma.restartServer', restartServers),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (restartingSettings.some((setting) => event.affectsConfiguration(setting))) {
        void restartServers()
      }
    }),
  )
  await startServers()
}

export async function deactivate(): Promise<void> {
  await stopServers()
}

function configuration(): vscode.WorkspaceConfiguration {
  return vscode.workspace.getConfiguration(configurationSection)
}

/** A string setting, trimmed, or `fallback` when it is empty. */
function configuredString(name: string, fallback: string): string {
  return configuration().get<string>(name)?.trim() || fallback
}

/** The server for `.lm` files. */
function lmServer(): ServerDescription {
  const binary = configuredString('lmPath', defaultLmBinary)
  return {
    id: 'lemma',
    name: 'Lemma',
    binary,
    setting: 'lmPath',
    server: { command: binary, args: ['lsp'] },
    client: {
      documentSelector: [
        { scheme: 'file', language: 'lemma' },
        { scheme: 'untitled', language: 'lemma' },
      ],
    },
  }
}

/** The server whose diagnostics are what saved changes break. */
function checkServer(): ServerDescription {
  const binary = configuredString('lemmaPath', defaultLemmaBinary)
  return {
    id: 'lemma-check',
    name: 'Lemma Check',
    binary,
    setting: 'lemmaPath',
    server: { command: binary, args: ['check', '--lsp'] },
    client: {
      // Diagnostics arrive for any file, open or not; the server reads
      // files from disk and needs no document from the editor.
      documentSelector: [{ scheme: 'file' }],
      initializationOptions: { manifest: configuredString('checkOnChange.manifest', '') },
    },
  }
}

/** How to start one server, and the setting naming its binary. */
interface ServerDescription {
  id: string
  name: string
  binary: string
  setting: string
  server: ServerOptions
  client: LanguageClientOptions
}

async function startServers(): Promise<void> {
  const wanted = [lmServer()]
  if (configuration().get<boolean>('checkOnChange.enabled') === true) {
    wanted.push(checkServer())
  }
  for (const description of wanted) {
    const started = await startServer(description)
    if (started !== undefined) {
      clients.push(started)
    }
  }
}

async function startServer(description: ServerDescription): Promise<LanguageClient | undefined> {
  const started = new LanguageClient(
    description.id,
    description.name,
    description.server,
    description.client,
  )
  try {
    await started.start()
    return started
  } catch (error) {
    // Highlighting still works without a server, so report the failure
    // and carry on rather than failing activation.
    const reason = error instanceof Error ? error.message : String(error)
    const openSettings = 'Open Settings'
    const setting = `${configurationSection}.${description.setting}`
    const choice = await vscode.window.showErrorMessage(
      `Lemma: could not start \`${description.binary}\` (${reason}). Set ${setting} to the binary.`,
      openSettings,
    )
    if (choice === openSettings) {
      await vscode.commands.executeCommand('workbench.action.openSettings', setting)
    }
    return undefined
  }
}

async function stopServers(): Promise<void> {
  const running = clients
  clients = []
  await Promise.all(running.map((client) => client.stop()))
}

async function restartServers(): Promise<void> {
  await stopServers()
  await startServers()
}
