// Lemma's adapter for Hono apps built with @hono/zod-openapi: it imports
// the app and writes the OpenAPI document its routes describe, as the
// framework itself would generate it. The protocol is in docs/adapters.md.
//
// Run with bun from the directory whose node_modules hold the app's
// packages:
//   LEMMA_OUTPUT=<file> LEMMA_ENTRY=<entries, one per line> bun run hono-zod-openapi.ts
//
// An entry is either the module exporting the app the server serves,
// `src/index.ts#app` (`#default` for a default export), or a directory or
// file of route modules to import one by one. The app is the better entry:
// its document has every route under the path it is served at, through
// `app.route('/users', users)` mounts and `basePath('/api')`. Route modules
// alone see each route only as its own router registers it.
//
// Every registration (OpenAPIRegistry.registerPath) is recorded where the
// app makes it, so each operation carries `x-lemma-handler`: the file and
// line of the route's first registration. A router mounted under another
// registers its routes again under the joined path; those keep the first
// registration's place. Modules that fail to import and routes that cannot
// be described are listed in `x-lemma-unread`; with none of the former,
// `x-lemma-complete` claims every route was read. With no route at all,
// the adapter fails.

import { readdirSync, statSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join, relative, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const output = process.env.LEMMA_OUTPUT
const lines = (value: string | undefined) => (value ?? '').split('\n').map((line) => line.trim()).filter(Boolean)
const entries = lines(process.env.LEMMA_ENTRY)
const excluded = lines(process.env.LEMMA_EXCLUDE).map(globPattern)
const root = resolve(process.env.LEMMA_ROOT ?? process.cwd())
if (!output || entries.length === 0) {
  console.error('LEMMA_OUTPUT and LEMMA_ENTRY are required')
  process.exit(2)
}

type Entry = { path: string; app?: string }
const parsed: Entry[] = entries.map((entry) => {
  const [path, app] = entry.split('#')
  return { path: resolve(path), app }
})

// The framework's own modules, resolved as the app resolves them, so the
// registry patched here is the one the app's routers use.
const fromApp = createRequire(join(parsed[0].path, statSync(parsed[0].path).isFile() ? '..' : '.', 'package.json'))
const fromHono = createRequire(fromApp.resolve('@hono/zod-openapi'))
const zodToOpenapi = await import(pathToFileURL(fromHono.resolve('@asteasolutions/zod-to-openapi')).href)

type Place = { file: string; line: number }
type Registration = { route: Record<string, any>; place?: Place }
const registrations: Registration[] = []

// A mounted router's routes are registered again as copies of their
// definitions under the joined path (`{ ...route, path }`): every other
// field is the very same value. Two routes written separately differ in at
// least one, even when they share a `responses` constant.
function isCopy(one: Record<string, any>, other: Record<string, any>): boolean {
  const keys = Object.keys(one).filter((key) => key !== 'path')
  const otherKeys = Object.keys(other).filter((key) => key !== 'path')
  return keys.length === otherKeys.length && keys.every((key) => one[key] === other[key])
}

// Whether `path` is where a router mounted somewhere serves its route at
// `own`: `/users/{id}` for `/{id}`, `/users` for `/`.
function isMountedAt(path: string, own: string): boolean {
  return own === '/' || path === own || path.endsWith(own)
}

// The place of a route's first registration, which a mounted copy keeps.
function firstPlace(route: Record<string, any>): Place | undefined {
  return registrations.find((earlier) => isCopy(earlier.route, route) && isMountedAt(route.path, earlier.route.path))?.place
}

const registerPath = zodToOpenapi.OpenAPIRegistry.prototype.registerPath
zodToOpenapi.OpenAPIRegistry.prototype.registerPath = function (route: Record<string, any>) {
  registrations.push({ route, place: firstPlace(route) ?? registeredAt(new Error().stack ?? '') })
  return registerPath.call(this, route)
}

const routeDirectories = parsed.filter((entry) => !entry.app).map((entry) => entry.path)

// Where the app registered a route: the first frame in a route module
// (under a directory entry), so a shared helper the route modules call is
// passed over for the route's own module; else the first frame outside
// node_modules and this adapter.
function registeredAt(stack: string): Place | undefined {
  const frames: Place[] = []
  for (const frame of stack.split('\n').slice(1)) {
    // Bun prints a module's top-level frame without a column.
    const match = frame.match(/((?:file:\/\/)?\/.*?):(\d+)(?::\d+)?\)?\s*$/)
    if (!match) continue
    const file = match[1].startsWith('file://') ? fileURLToPath(match[1]) : match[1]
    if (file.includes('/node_modules/') || file.endsWith('hono-zod-openapi.ts')) continue
    frames.push({ file, line: Number(match[2]) })
  }
  const inRoutes = frames.find(({ file }) => routeDirectories.some((directory) => file === directory || file.startsWith(`${directory}/`)))
  const chosen = inRoutes ?? frames[0]
  return chosen && { file: relative(root, chosen.file), line: chosen.line }
}

function globPattern(glob: string): RegExp {
  const pattern = glob
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*\*\/?/g, '\u0000')
    .replace(/\*/g, '[^/]*')
    .replace(/\?/g, '[^/]')
    .replace(/\u0000/g, '.*')
  return new RegExp(`^${pattern}$`)
}

const isTest = (path: string) => /\.(test|spec|d|e2e)\.|__tests__|\/tests?\//.test(path)

function modules(entry: string): string[] {
  if (statSync(entry).isFile()) return [entry]
  const found: string[] = []
  for (const name of readdirSync(entry).sort()) {
    if (name === 'node_modules' || name.startsWith('.')) continue
    const child = join(entry, name)
    if (excluded.some((pattern) => pattern.test(relative(process.cwd(), child)))) continue
    if (statSync(child).isDirectory()) {
      found.push(...modules(child))
    } else if (/\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/.test(name) && !isTest(child)) {
      found.push(child)
    }
  }
  return found
}

const unread: Record<string, string>[] = []
async function load(module: string): Promise<Record<string, any> | undefined> {
  try {
    return await import(pathToFileURL(module).href)
  } catch (error) {
    const reason = (error as Error).message?.split('\n')[0] ?? String(error)
    unread.push({ module: relative(root, module), reason })
    console.error(`lemma: could not import ${relative(root, module)}: ${reason}`)
    return undefined
  }
}

// The routes to describe: each app's own registry (every route, mounted,
// under its base path), and every registration in route modules.
type Described = { route: Record<string, any>; place?: Place }
const described: Described[] = []
for (const entry of parsed) {
  if (!entry.app) {
    for (const module of modules(entry.path)) await load(module)
    continue
  }
  const exports = await load(entry.path)
  const app = exports?.[entry.app]
  if (!app?.openAPIRegistry) {
    if (exports) unread.push({ module: relative(root, entry.path), reason: `exports no OpenAPIHono app named ${entry.app}` })
    continue
  }
  const basePath = String(app._basePath ?? '').replace(/:([^/]+)/g, '{$1}')
  for (const definition of app.openAPIRegistry.definitions) {
    if (definition.type !== 'route') continue
    const route = { ...definition.route, path: joinPaths(basePath, definition.route.path) }
    described.push({ route, place: firstPlace(definition.route) })
  }
}
// With an app, route modules only say where routes are registered: the
// app's registry already holds them, mounted and under its base path.
const hasApp = parsed.some((entry) => entry.app)
if (!hasApp) described.push(...servedRegistrations())

function joinPaths(base: string, path: string): string {
  const joined = `/${base}/${path}`.replace(/\/+/g, '/')
  return joined.length > 1 ? joined.replace(/\/$/, '') : joined
}

// Registrations in route modules, less the paths a router registers for
// itself before it is mounted: a route registered at `/x` and, mounted,
// at `/users/x` is served at the latter.
function servedRegistrations(): Described[] {
  return registrations.filter(({ route }) =>
    !registrations.some(({ route: other }) => other.path !== route.path && isMountedAt(other.path, route.path) && isCopy(other, route)),
  )
}

// Each route is described on its own, so one schema the generator cannot
// describe (a z.lazy without OpenAPI metadata) loses only its route.
const document: Record<string, any> = {
  openapi: '3.0.0',
  info: { title: 'generated by Lemma', version: '0' },
  paths: {},
  components: {},
}
for (const { route, place } of described) {
  let part
  try {
    part = new zodToOpenapi.OpenApiGeneratorV3([{ type: 'route', route }]).generateDocument({
      openapi: '3.0.0',
      info: { title: '', version: '0' },
    })
  } catch (error) {
    const reason = (error as Error).message ?? String(error)
    unread.push({ method: String(route.method).toUpperCase(), path: String(route.path).replace(/:([^/]+)/g, '{$1}'), reason })
    console.error(`lemma: could not describe ${String(route.method).toUpperCase()} ${route.path}: ${reason}`)
    continue
  }
  for (const [path, item] of Object.entries(part.paths ?? {})) {
    for (const [method, operation] of Object.entries(item as Record<string, any>)) {
      // A route registered again at the same path (by a module that
      // collects every route's definition into a document) is one route.
      if (document.paths[path]?.[method]) continue
      if (place) operation['x-lemma-handler'] = `${place.file}:${place.line}`
      document.paths[path] = { ...document.paths[path], [method]: operation }
    }
  }
  for (const [kind, components] of Object.entries(part.components ?? {})) {
    document.components[kind] = { ...document.components[kind], ...(components as object) }
  }
}
document['x-lemma-unread'] = unread
document['x-lemma-complete'] = !unread.some((entry) => 'module' in entry)
writeFileSync(output, JSON.stringify(document, null, 2))
const count = Object.values(document.paths).reduce((total: number, item) => total + Object.keys(item as object).length, 0)
console.error(`lemma: ${count} operations from ${entries.length} entries; ${unread.length} things could not be read`)
if (count === 0) {
  console.error(`lemma: no route was registered${unread.length ? `; first: ${JSON.stringify(unread[0])}` : ''}`)
  process.exit(3)
}
process.exit(0)
