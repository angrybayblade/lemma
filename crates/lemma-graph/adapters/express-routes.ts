// Lemma's adapter for Express apps: it imports the app and writes an
// OpenAPI document listing the routes it serves. Express knows a route's
// method and path but not its parameters, bodies or responses, so the
// document says `x-lemma-schemas: false` and the committed documents keep
// the schemas. The protocol is in docs/adapters.md.
//
// Run with bun from the directory whose node_modules hold the app's
// packages:
//   LEMMA_OUTPUT=<file> LEMMA_ENTRY=<entries, one per line> bun run express-routes.ts
//
// Bun rather than Node: it imports TypeScript and JavaScript, ES modules
// and CommonJS alike without a build step, and Express apps come in all
// four.
//
// An entry is the module exporting the app the server serves,
// `src/app.js#app` (`#default` for a default export or `module.exports`);
// an exported function taking no arguments is called to make the app. An
// exported Router is read as if served at `/`. A module without `#` has
// every app it exports read. A directory is imported module by module:
// apps its modules export are read when no entry names one, and a route
// registered through a shared helper is placed at the module under it
// that calls the helper.
//
// The adapter records every registration as the app makes it, wherever
// the Express in use keeps it (Express 4 and 5 both): `router.get(...)`,
// `app.route('/x').get(...).post(...)`, and routers and apps mounted with
// `use('/prefix', ...)`, nested to any depth. Each operation carries
// `x-lemma-handler`, the file and line of the call registering it. The
// app's server is never started: `listen` is replaced by a stub.
//
// Routes whose path Express matches by pattern (a RegExp, a wildcard, an
// optional part) and routes registered for every method (`all`) cannot be
// described; they are listed in `x-lemma-unread`. A RegExp route serves
// paths the adapter cannot name, so it also leaves the reading incomplete,
// as does any module that fails to import. With no route at all, the
// adapter fails.

import { readdirSync, statSync, writeFileSync } from 'node:fs'
import http, { METHODS } from 'node:http'
import https from 'node:https'
import { createRequire } from 'node:module'
import net from 'node:net'
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

type Entry = { path: string; name?: string }
const parsed: Entry[] = entries.map((entry) => {
  const [path, name] = entry.split('#')
  return { path: resolve(path), name }
})
const routeDirectories = parsed.filter((entry) => isDirectory(entry.path)).map((entry) => entry.path)

type Place = { file: string; line: number }
type RouteRecord = { method: string; path: unknown; place?: Place }
type MountRecord = { path: unknown; child: any }

// What each router registered, and what each router or app mounted.
const routesOf = new WeakMap<object, RouteRecord[]>()
const mountsOf = new WeakMap<object, MountRecord[]>()
// The router each Route object belongs to.
const ownerOf = new WeakMap<object, object>()
// Inside `app.all(...)`, which registers the route once per method.
let allDepth = 0

function push<T>(map: WeakMap<object, T[]>, key: object, value: T) {
  const list = map.get(key) ?? []
  list.push(value)
  map.set(key, list)
}

// The Express the app imports, resolved as the app resolves it, so the
// prototypes patched here are the ones its routers use.
const firstModule = parsed[0].path
const fromApp = createRequire(join(firstModule, isDirectory(firstModule) ? '.' : '..', 'package.json'))
let express: any
try {
  express = fromApp('express')
} catch (error) {
  console.error(`lemma: express cannot be resolved from ${relative(root, firstModule)}: ${(error as Error).message.split('\n')[0]}`)
  process.exit(3)
}
patchExpress(express)
stubListen(express)

// Express 5 keeps its Router in the `router` package, whose methods are on
// `Router.prototype`; Express 4's Router is itself the object routers
// inherit from. `express.Route` is the Route class in both.
function patchExpress(express: any) {
  const routerProto = typeof express.Router.prototype?.use === 'function' ? express.Router.prototype : express.Router
  const routeProto = express.Route.prototype
  const application = express.application

  const route = routerProto.route
  routerProto.route = function (this: object, path: unknown) {
    const created = route.call(this, path)
    ownerOf.set(created, this)
    return created
  }

  const use = routerProto.use
  routerProto.use = function (this: object, ...args: unknown[]) {
    const { path, callbacks } = useArguments(args)
    for (const callback of callbacks) {
      if (isRouter(callback)) push(mountsOf, this, { path, child: callback })
    }
    return use.apply(this, args)
  }

  // An app mounted on an app is wrapped in a function before it reaches
  // the router, so it is recorded here, on the app.
  const appUse = application.use
  application.use = function (this: object, ...args: unknown[]) {
    const { path, callbacks } = useArguments(args)
    for (const callback of callbacks) {
      if (isApp(callback)) push(mountsOf, this, { path, child: callback })
    }
    return appUse.apply(this, args)
  }

  const appAll = application.all
  application.all = function (this: object, ...args: unknown[]) {
    allDepth += 1
    try {
      return appAll.apply(this, args)
    } finally {
      allDepth -= 1
    }
  }

  const methods = [...METHODS.map((method) => method.toLowerCase()), 'all']
  for (const method of methods) {
    const original = routeProto[method]
    if (typeof original !== 'function') continue
    routeProto[method] = function (this: { path: unknown }, ...args: unknown[]) {
      recordRoute(this, allDepth > 0 ? 'all' : method)
      return original.apply(this, args)
    }
  }
}

function recordRoute(route: { path: unknown }, method: string) {
  const owner = ownerOf.get(route)
  if (!owner) return
  const records = routesOf.get(owner) ?? []
  // `app.all` and `route.get(a).get(b)` call the same Route more than
  // once; the first call is the registration.
  if (records.some((record) => record.method === method && record.path === route.path)) return
  push(routesOf, owner, { method, path: route.path, place: registeredAt(new Error().stack ?? '') })
}

// `use([path,] ...callbacks)`, read as Express reads it.
function useArguments(args: unknown[]): { path: unknown; callbacks: unknown[] } {
  let first = args[0]
  while (Array.isArray(first) && first.length > 0) first = first[0]
  const hasPath = typeof first !== 'function'
  return { path: hasPath ? args[0] : '/', callbacks: args.slice(hasPath ? 1 : 0).flat(Infinity) }
}

function isApp(value: any): boolean {
  return typeof value === 'function' && typeof value.handle === 'function' && typeof value.set === 'function'
}

function isRouter(value: any): boolean {
  return typeof value === 'function' && Array.isArray(value.stack) && typeof value.route === 'function'
}

// The router an app keeps its routes in: `app.router` in Express 5,
// `app._router` (made on first use) in Express 4.
function routerOf(app: any): object | undefined {
  if ('_router' in app || typeof app.lazyrouter === 'function') return app._router
  return app.router
}

// Importing an app must not open a port: `app.listen` and the servers made
// for it answer as if listening, and never are.
function stubListen(express: any) {
  express.application.listen = function (this: any) {
    const server = new net.Server()
    console.error('lemma: app.listen was called while importing; the server was not started')
    return server
  }
  // Bun's http and https servers do not inherit net.Server's listen.
  for (const server of [net.Server, http.Server, https.Server]) {
    server.prototype.listen = function (this: net.Server) {
      console.error('lemma: a server tried to listen while importing; it was not started')
      return this
    }
  }
}

// Where the app registered a route: the first frame in a route module
// (under a directory entry), so a shared helper the route modules call is
// passed over for the route's own module; else the first frame outside
// node_modules and this adapter.
// Bun links node_modules into its own cache, which is where a frame says it is.
function isDependency(file: string): boolean {
  return file.includes('/node_modules/') || file.includes('/.bun/')
}

function registeredAt(stack: string): Place | undefined {
  const frames: Place[] = []
  for (const frame of stack.split('\n').slice(1)) {
    const match = frame.match(/((?:file:\/\/)?\/.*?):(\d+):\d+\)?\s*$/)
    if (!match) continue
    const file = match[1].startsWith('file://') ? fileURLToPath(match[1]) : match[1]
    if (isDependency(file) || file.endsWith('express-routes.ts')) continue
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

function isDirectory(path: string): boolean {
  try {
    return statSync(path).isDirectory()
  } catch {
    return false
  }
}

const isTest = (path: string) => /\.(test|spec|d|e2e)\.|__tests__|\/tests?\//.test(path)

function modules(entry: string): string[] {
  if (!isDirectory(entry)) return [entry]
  const found: string[] = []
  for (const name of readdirSync(entry).sort()) {
    if (name === 'node_modules' || name.startsWith('.')) continue
    const child = join(entry, name)
    if (excluded.some((pattern) => pattern.test(relative(process.cwd(), child)))) continue
    if (isDirectory(child)) {
      found.push(...modules(child))
    } else if (/\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/.test(name) && !isTest(child)) {
      found.push(child)
    }
  }
  return found
}

type Unread = { module: string; reason: string } | { method: string; path: string; reason: string; prefix?: boolean }
const unread: Unread[] = []
let hasUnlistedRoutes = false

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

// The app or router an entry names: the export itself, or what an
// exported function taking no arguments returns.
async function served(exports: Record<string, any>, name: string): Promise<any> {
  const value = exports[name]
  if (isApp(value) || isRouter(value)) return value
  if (typeof value === 'function' && value.length === 0) return await value()
  return value
}

// Every app (or router) to read, with the module it came from.
const targets: { target: any; module: string }[] = []
const namesAnApp = parsed.some((entry) => entry.name || !isDirectory(entry.path))
for (const entry of parsed) {
  if (isDirectory(entry.path)) {
    for (const module of modules(entry.path)) {
      const exports = await load(module)
      if (exports && !namesAnApp) targets.push(...exportedApps(exports).map((target) => ({ target, module })))
    }
    continue
  }
  const exports = await load(entry.path)
  if (!exports) continue
  if (!entry.name) {
    targets.push(...exportedApps(exports).map((target) => ({ target, module: entry.path })))
    continue
  }
  let target
  try {
    target = await served(exports, entry.name)
  } catch (error) {
    unread.push({ module: relative(root, entry.path), reason: `${entry.name}() failed: ${(error as Error).message}` })
    continue
  }
  if (!isApp(target) && !isRouter(target)) {
    unread.push({ module: relative(root, entry.path), reason: `exports no Express app or router named ${entry.name}` })
    continue
  }
  targets.push({ target, module: entry.path })
}

function exportedApps(exports: Record<string, any>): any[] {
  return [...new Set(Object.values(exports).filter(isApp))]
}

// A path as OpenAPI writes it, or why it cannot be: Express's `:id` is
// `{id}`; anything Express matches by pattern is not one path.
// `under` is the static part of a pattern path (`/files` for
// `/files/*path`): a pattern may serve any path below it, so committed
// operations there must not count as removed.
type Described = { path: string } | { shown: string; reason: string; isNamed: boolean; under?: string }

function describePath(raw: unknown): Described[] {
  if (Array.isArray(raw)) return raw.flatMap(describePath)
  if (raw instanceof RegExp) return [{ shown: String(raw), reason: 'matched by a regular expression', isNamed: false }]
  if (typeof raw !== 'string') return [{ shown: String(raw), reason: 'a path Express was given as neither text nor a pattern', isNamed: false }]
  const segments = raw.split('/').filter(Boolean)
  const described: string[] = []
  for (const [index, segment] of segments.entries()) {
    const parameter = segment.match(/^:([A-Za-z_$][\w$]*)$/)
    if (parameter) {
      described.push(`{${parameter[1]}}`)
      continue
    }
    if (/[:*?+()[\]{}!\\]/.test(segment)) {
      const under = `/${segments.slice(0, index).map((part) => part.replace(/^:([A-Za-z_$][\w$]*)$/, '{$1}')).join('/')}`
      return [{ shown: approximate(raw), reason: `the path ${raw} is a pattern (a wildcard, an optional part or a regular expression)`, isNamed: true, under }]
    }
    described.push(segment)
  }
  return [{ path: `/${described.join('/')}` }]
}

// A pattern path written as nearly as OpenAPI can, so a committed
// operation it serves can be recognised: `:id` and `*rest` as parameters.
function approximate(raw: string): string {
  const path = raw
    .replace(/\{([^}]*)\}/g, '$1')
    .replace(/\(.*?\)/g, '')
    .replace(/[:*]([A-Za-z_$][\w$]*)\??/g, '{$1}')
    .replace(/\*/g, '{wildcard}')
    .replace(/[?+]/g, '')
  return path.startsWith('/') ? path : `/${path}`
}

function joinPaths(base: string, path: string): string {
  const joined = `/${base}/${path}`.replace(/\/+/g, '/')
  return joined.length > 1 ? joined.replace(/\/$/, '') : joined
}

type Operation = { method: string; path: string; place?: Place }
const operations: Operation[] = []
const commonMethods = ['get', 'put', 'post', 'delete', 'patch', 'head', 'options']

function noteUnread(method: string, described: Extract<Described, { shown: string }>, prefix: string) {
  if (!described.isNamed) hasUnlistedRoutes = true
  if (described.under !== undefined) {
    unread.push({ method: method.toUpperCase(), path: joinPaths(prefix, described.under), prefix: true, reason: described.reason })
    return
  }
  const path = described.isNamed ? joinPaths(prefix, described.shown) : `${prefix}${described.shown}`
  unread.push({ method: method.toUpperCase(), path, reason: described.reason })
}

// Every route under `target`, at the path it is served at.
function collect(target: any, prefix: string, visiting: Set<object>) {
  if (visiting.has(target)) return
  visiting.add(target)
  const router = isApp(target) ? routerOf(target) : target
  const owners = [target, router].filter((owner, index, all) => owner && all.indexOf(owner) === index)
  for (const owner of owners) {
    for (const mount of mountsOf.get(owner) ?? []) {
      for (const described of describePath(mount.path)) {
        if ('path' in described) {
          collect(mount.child, joinPaths(prefix, described.path), visiting)
          continue
        }
        // The mounted routes are not read at all, so some may be missed.
        hasUnlistedRoutes = true
        noteUnread('*', described, prefix)
      }
    }
  }
  for (const record of (router && routesOf.get(router)) ?? []) {
    for (const described of describePath(record.path)) {
      if (!('path' in described)) {
        noteUnread(record.method === 'all' ? '*' : record.method, described, prefix)
        continue
      }
      const path = joinPaths(prefix, described.path)
      if (record.method === 'all') {
        for (const method of commonMethods) unread.push({ method: method.toUpperCase(), path, reason: 'registered for every method with all()' })
        continue
      }
      operations.push({ method: record.method, path, place: record.place })
    }
  }
  visiting.delete(target)
}

for (const { target } of targets) collect(target, '', new Set())

// Sorted, so the same code always gives the same document.
const document: Record<string, any> = {
  openapi: '3.0.0',
  info: { title: 'generated by Lemma', version: '0' },
  paths: {},
}
operations.sort((one, other) => one.path.localeCompare(other.path) || one.method.localeCompare(other.method))
for (const { method, path, place } of operations) {
  if (document.paths[path]?.[method]) continue
  const parameters = [...path.matchAll(/\{([^}]+)\}/g)].map((match) => ({ name: match[1], in: 'path', required: true, schema: { type: 'string' } }))
  const operation: Record<string, unknown> = parameters.length ? { parameters } : {}
  if (place) operation['x-lemma-handler'] = `${place.file}:${place.line}`
  document.paths[path] = { ...document.paths[path], [method]: operation }
}
const unreadKey = (entry: Unread) => ('module' in entry ? `0 ${entry.module}` : `1 ${entry.path} ${entry.method}`)
const uniqueUnread = [...new Map(unread.map((entry) => [JSON.stringify(entry), entry])).values()]
uniqueUnread.sort((one, other) => unreadKey(one).localeCompare(unreadKey(other)))
document['x-lemma-unread'] = uniqueUnread
document['x-lemma-complete'] = targets.length > 0 && !hasUnlistedRoutes && !uniqueUnread.some((entry) => 'module' in entry)
document['x-lemma-schemas'] = false
writeFileSync(output, JSON.stringify(document, null, 2))
const count = operations.length
console.error(`lemma: ${Object.values(document.paths).reduce((total: number, item) => total + Object.keys(item as object).length, 0)} operations from ${entries.length} entries; ${uniqueUnread.length} things could not be read`)
if (count === 0) {
  console.error(`lemma: no Express route was registered${uniqueUnread.length ? `; first: ${JSON.stringify(uniqueUnread[0])}` : ''}`)
  process.exit(3)
}
process.exit(0)
