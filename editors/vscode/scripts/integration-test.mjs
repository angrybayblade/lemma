// Runs the extension in a real VS Code. It installs the packaged .vsix into a
// fresh profile, points `lemma.lmPath` at the `lm` built from this checkout,
// opens editors/fixture and runs the Mocha suite in out-test/suite inside
// VS Code, which asserts diagnostics, hover, go-to-definition and TextMate
// tokens.
//
// Usage: npm run test:integration (which packages and compiles first)
//
// Environment:
//   LM                 the lm binary; defaults to target/release/lm
//   VSCODE_EXECUTABLE  an installed VS Code to use instead of downloading one
//   VSCODE_VERSION     the VS Code to download, `stable` by default
//
// Exits with 77 (the usual "skipped" status) when it cannot run at all: no
// display and no xvfb-run, or no VS Code to download.

import { spawnSync } from 'node:child_process'
import {
  accessSync,
  constants,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  downloadAndUnzipVSCode,
  resolveCliPathFromVSCodeExecutablePath,
  runTests,
} from '@vscode/test-electron'

const skipped = 77

const extensionDirectory = join(dirname(fileURLToPath(import.meta.url)), '..')
const repository = join(extensionDirectory, '..', '..')
const fixture = join(repository, 'editors', 'fixture')
const manifest = JSON.parse(readFileSync(join(extensionDirectory, 'package.json'), 'utf8'))
const vsix = join(extensionDirectory, `${manifest.name}-${manifest.version}.vsix`)
const extensionId = `${manifest.publisher}.${manifest.name}`
const suite = join(extensionDirectory, 'out-test', 'suite', 'index.js')

function skip(reason) {
  console.error(`Skipping the VS Code integration test: ${reason}`)
  process.exit(skipped)
}

function fail(reason) {
  console.error(`VS Code integration test: ${reason}`)
  process.exit(1)
}

function isExecutable(path) {
  try {
    accessSync(path, constants.X_OK)
    return true
  } catch {
    return false
  }
}

/** Whether VS Code needs a virtual display here: Linux with no display. */
function needsVirtualDisplay() {
  const hasDisplay = Boolean(process.env.DISPLAY || process.env.WAYLAND_DISPLAY)
  return process.platform === 'linux' && !hasDisplay
}

/** Runs this script again under xvfb-run, and exits with its status. */
function rerunUnderXvfb() {
  const script = fileURLToPath(import.meta.url)
  const result = spawnSync('xvfb-run', ['--auto-servernum', process.execPath, script], {
    stdio: 'inherit',
  })
  if (result.error?.code === 'ENOENT') {
    skip('there is no display and xvfb-run is not installed')
  }
  if (result.error) {
    fail(`could not run xvfb-run: ${result.error.message}`)
  }
  process.exit(result.status ?? 1)
}

function lemmaBinary() {
  const targetDirectory = process.env.CARGO_TARGET_DIR || join(repository, 'target')
  const binary = process.env.LM || join(targetDirectory, 'release', 'lm')
  if (!isExecutable(binary)) {
    fail(`no lm binary at ${binary}; run \`cargo build --release -p lemma-cli --bin lm\` or set LM`)
  }
  return binary
}

async function vscodeExecutable() {
  if (process.env.VSCODE_EXECUTABLE) {
    return process.env.VSCODE_EXECUTABLE
  }
  try {
    return await downloadAndUnzipVSCode({
      version: process.env.VSCODE_VERSION || 'stable',
      cachePath: join(extensionDirectory, '.vscode-test'),
    })
  } catch (error) {
    skip(
      `VS Code could not be downloaded (${error.message}); set VSCODE_EXECUTABLE to use an installed one`,
    )
  }
}

/** A fresh profile: settings that use this checkout's server, no extensions. */
function makeProfile(scratch, binary) {
  const profile = {
    userData: join(scratch, 'user-data'),
    extensions: join(scratch, 'extensions'),
  }
  mkdirSync(join(profile.userData, 'User'), { recursive: true })
  mkdirSync(profile.extensions, { recursive: true })
  const settings = {
    'lemma.lmPath': binary,
    'extensions.autoCheckUpdates': false,
    'extensions.autoUpdate': false,
    'telemetry.telemetryLevel': 'off',
    'update.mode': 'none',
    'workbench.startupEditor': 'none',
    // VS Code's built-in chat otherwise fills the log with sign-in messages.
    'chat.disableAIFeatures': true,
  }
  writeFileSync(join(profile.userData, 'User', 'settings.json'), JSON.stringify(settings, null, 2))
  return profile
}

/** Runs VS Code's command line against `profile`, returning its output. */
function runCli(executable, profile, args) {
  const cli = resolveCliPathFromVSCodeExecutablePath(executable)
  const profileArgs = [
    `--extensions-dir=${profile.extensions}`,
    `--user-data-dir=${profile.userData}`,
  ]
  const result = spawnSync(cli, [...profileArgs, ...args], { encoding: 'utf8' })
  if (result.status !== 0) {
    throw new Error(`\`code ${args.join(' ')}\` failed:\n${result.stdout}${result.stderr}`)
  }
  return result.stdout
}

function installPackage(executable, profile) {
  process.stdout.write(runCli(executable, profile, ['--install-extension', vsix]))
  const installed = runCli(executable, profile, ['--list-extensions', '--show-versions'])
  if (!installed.includes(`${extensionId}@${manifest.version}`)) {
    throw new Error(`${vsix} installed, but VS Code lists only:\n${installed}`)
  }
}

/**
 * VS Code runs a test suite only beside an extension under development. The
 * extension under test is the installed package, so this stand-in is empty.
 */
function makeTestHost(scratch) {
  const host = join(scratch, 'host')
  mkdirSync(host)
  const hostManifest = {
    name: 'lemma-test-host',
    publisher: 'lemma',
    version: '0.0.0',
    engines: { vscode: '*' },
  }
  writeFileSync(join(host, 'package.json'), JSON.stringify(hostManifest, null, 2))
  return host
}

async function main() {
  if (needsVirtualDisplay()) {
    rerunUnderXvfb()
  }
  const binary = lemmaBinary()
  if (!existsSync(vsix)) {
    fail(`no package at ${vsix}; run \`npm run package\``)
  }
  const executable = await vscodeExecutable()
  const scratch = mkdtempSync(join(tmpdir(), 'lemma-vscode-'))
  let failure
  try {
    const profile = makeProfile(scratch, binary)
    installPackage(executable, profile)
    await runTests({
      vscodeExecutablePath: executable,
      extensionDevelopmentPath: makeTestHost(scratch),
      extensionTestsPath: suite,
      extensionTestsEnv: { LEMMA_EXTENSIONS_DIR: profile.extensions },
      launchArgs: [
        fixture,
        `--extensions-dir=${profile.extensions}`,
        `--user-data-dir=${profile.userData}`,
        '--disable-gpu',
      ],
    })
  } catch (error) {
    failure = error
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
  if (failure !== undefined) {
    fail(failure.message)
  }
}

await main()
