// The entry point VS Code calls in the extension host when it runs with
// --extensionTestsPath: it runs every compiled `*.test.js` beside this file
// with Mocha and fails the run if any test fails.

import { readdirSync } from 'node:fs'
import { join } from 'node:path'
import Mocha from 'mocha'

export async function run(): Promise<void> {
  // The language server starts on demand, so the first request can take a
  // few seconds on a cold machine.
  const mocha = new Mocha({ ui: 'bdd', color: true, timeout: 60_000 })
  const tests = readdirSync(__dirname)
    .filter((name) => name.endsWith('.test.js'))
    .sort()
  for (const test of tests) {
    mocha.addFile(join(__dirname, test))
  }
  const failures = await new Promise<number>((resolve) => mocha.run(resolve))
  if (failures > 0) {
    throw new Error(`${failures} VS Code integration test(s) failed`)
  }
}
