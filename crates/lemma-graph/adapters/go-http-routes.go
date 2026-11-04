//go:build ignore

// Lemma's adapter for Go services routed with gorilla/mux or net/http's
// ServeMux: it builds the service's router the way the service does and
// writes the routes it holds as an OpenAPI document. The protocol is in
// docs/adapters.md.
//
//	LEMMA_OUTPUT=<file> LEMMA_ENTRY=<entries, one per line> go run go-http-routes.go
//
// Run from inside the service's module. A script outside a module cannot
// import the module's packages, so this one only drives: it writes a small
// program into the module (marked with the build tag lemma_routes, removed
// afterwards), builds it with the module's own dependencies, runs it once
// per entry, and reads back what it found.
//
// An entry names what builds the router, by package import path or a
// directory relative to where the adapter runs:
//
//	./internal/http#NewRouter         a function; it is called
//	./internal/http#Server.Routes     a method; called on a zero Server
//	./internal/http#Router            a package-level variable
//	./cmd/api                         a package alone: its init functions
//	                                  run and http.DefaultServeMux is read
//
// A function is called with zero values for its parameters, except that a
// *mux.Router or *http.ServeMux parameter gets a fresh router, read after the
// call (so `func Setup(r *mux.Router, ...)` works), a context.Context gets
// context.Background(), and loggers (log, slog, zap, logrus, zerolog) get
// ones that discard. If the call panics, it is tried once more with every
// pointer pointing at a zero value instead of nil. Routers are found in what
// the function returns, through structs that embed or hold them. Unexported
// functions and functions in package main are read from a test file written
// into their package, since nothing else can reach them. Files constrained
// with `//go:build lemma` are built in, for a constructor written for Lemma
// that passes real no-op dependencies.
//
// It reads routes only (`x-lemma-schemas: false`). Each operation's
// `x-lemma-handler` is the handler's definition, or for ServeMux the line
// that registered the route when the handler is not in the code being read.
package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"unicode"
)

// generatedPrefix names everything this adapter writes into the module, so
// a copy left behind by a killed run is recognised and removed later.
const generatedPrefix = "lemma_routes_"

// sharedClosurePaths is how many distinct paths may share one closure as
// their handler before it is taken for a wrapper's (a middleware's) rather
// than a handler's, and dropped.
const sharedClosurePaths = 2

// entry is one configured `LEMMA_ENTRY` line, resolved.
type entry struct {
	text       string // as configured
	importPath string
	name       string // package name
	directory  string
	symbol     string // "", "Func", "Var" or "Type.Method"
}

// isTestMode reports whether the entry can only be reached from inside its
// own package.
func (e entry) isTestMode() bool {
	if e.name == "main" {
		return true
	}
	first, _, _ := strings.Cut(e.symbol, ".")
	return first != "" && !isExported(first)
}

// reference is the Go expression that yields the entry's value, with the
// package qualifier `qualifier` ("" inside the package).
func (e entry) reference(qualifier string) string {
	if e.symbol == "" {
		return "nil"
	}
	prefix := ""
	if qualifier != "" {
		prefix = qualifier + "."
	}
	if typeName, method, isMethod := strings.Cut(e.symbol, "."); isMethod {
		return fmt.Sprintf("(*%s%s).%s", prefix, typeName, method)
	}
	return prefix + e.symbol
}

// found is what the generated program reports for one entry.
type found struct {
	Routes []foundRoute      `json:"routes"`
	Unread []json.RawMessage `json:"unread"`
	Failed bool              `json:"failed"` // the call panicked, timed out or exited
}

// foundRoute is one route as the router holds it.
type foundRoute struct {
	Source     string   `json:"source"` // "mux" or "servemux"
	Pattern    string   `json:"pattern"`
	Methods    []string `json:"methods"`
	IsSubtree  bool     `json:"subtree"`
	Function   string   `json:"function"`
	File       string   `json:"file"`
	Line       int      `json:"line"`
	Registered string   `json:"registered"`
}

// operation is one route as the document gives it.
type operation struct {
	method  string
	path    string
	handler string
	site    string // handler site before wrappers are dropped
	fn      string
}

func main() {
	os.Exit(run())
}

func run() int {
	output := os.Getenv("LEMMA_OUTPUT")
	entryLines := nonEmptyLines(os.Getenv("LEMMA_ENTRY"))
	if output == "" || len(entryLines) == 0 {
		fmt.Fprintln(os.Stderr, "LEMMA_OUTPUT and LEMMA_ENTRY are required")
		return 2
	}
	root := realPath(firstNonEmpty(os.Getenv("LEMMA_ROOT"), "."))
	workingDirectory := realPath(".")
	moduleDirectory, err := moduleDirectoryOf(workingDirectory)
	if err != nil {
		fmt.Fprintf(os.Stderr, "lemma: %v\n", err)
		return 1
	}
	removeStaleCopies(moduleDirectory)
	entries, err := resolveEntries(entryLines, workingDirectory)
	if err != nil {
		fmt.Fprintf(os.Stderr, "lemma: %v\n", err)
		return 1
	}
	restore := keepModuleFiles(moduleDirectory)
	results, err := readEntries(entries, moduleDirectory, workingDirectory)
	restore()
	if err != nil {
		fmt.Fprintf(os.Stderr, "lemma: %v\n", err)
		return 1
	}
	document, count := buildDocument(entries, results, root)
	text, err := json.MarshalIndent(document, "", "  ")
	if err != nil {
		fmt.Fprintf(os.Stderr, "lemma: %v\n", err)
		return 1
	}
	if err := os.WriteFile(output, text, 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "lemma: %v\n", err)
		return 1
	}
	fmt.Fprintf(os.Stderr, "lemma: %d operations from %d entries\n", count, len(entries))
	if count == 0 {
		fmt.Fprintf(os.Stderr, "lemma: %s register no routes\n", strings.Join(entryLines, ", "))
		return 3
	}
	return 0
}

// moduleDirectoryOf is the directory of the go.mod governing `directory`.
func moduleDirectoryOf(directory string) (string, error) {
	text, err := goCommand(directory, nil, "env", "GOMOD")
	if err != nil {
		return "", err
	}
	goMod := strings.TrimSpace(text)
	if goMod == "" || goMod == os.DevNull {
		return "", fmt.Errorf("%s is not inside a Go module", directory)
	}
	return filepath.Dir(goMod), nil
}

// resolveEntries finds each entry's package with one `go list`.
func resolveEntries(lines []string, workingDirectory string) ([]entry, error) {
	var patterns []string
	entries := make([]entry, len(lines))
	for index, line := range lines {
		pattern, symbol, _ := strings.Cut(line, "#")
		entries[index] = entry{text: line, symbol: strings.TrimSpace(symbol)}
		patterns = append(patterns, strings.TrimSpace(pattern))
	}
	arguments := append([]string{"list", "-e", "-f", "{{.ImportPath}}\t{{.Name}}\t{{.Dir}}\t{{if .Error}}{{.Error}}{{end}}"}, patterns...)
	text, err := goCommand(workingDirectory, nil, arguments...)
	if err != nil {
		return nil, err
	}
	listed := nonEmptyLines(text)
	if len(listed) != len(entries) {
		return nil, fmt.Errorf("go list gave %d packages for %d entries", len(listed), len(entries))
	}
	for index, line := range listed {
		// Lines are trimmed, so a package without an error has three fields.
		fields := strings.SplitN(line, "\t", 4)
		if len(fields) < 3 || len(fields) == 4 || fields[1] == "" {
			return nil, fmt.Errorf("entry %q names no package: %s", entries[index].text, line)
		}
		entries[index].importPath, entries[index].name, entries[index].directory = fields[0], fields[1], fields[2]
	}
	return entries, nil
}

// readEntries builds the generated programs and runs each entry in a
// process of its own, so one entry's panic, os.Exit or registration on
// http.DefaultServeMux cannot touch another's.
func readEntries(entries []entry, moduleDirectory, workingDirectory string) ([]found, error) {
	scratch, err := os.MkdirTemp("", "lemma-go-routes-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(scratch)
	known := knownTypes(moduleDirectory)
	results := make([]found, len(entries))
	var inPackage []int
	var inMain []int
	for index, e := range entries {
		if e.isTestMode() {
			inPackage = append(inPackage, index)
			continue
		}
		inMain = append(inMain, index)
	}
	if len(inMain) > 0 {
		binary, err := buildMainProgram(entries, inMain, known, moduleDirectory, scratch)
		if err != nil {
			return nil, err
		}
		for position, index := range inMain {
			results[index] = runEntry(binary, nil, position, entries[index].text, workingDirectory, scratch)
		}
	}
	for _, group := range byPackage(entries, inPackage) {
		binary, err := buildTestProgram(entries, group, known, moduleDirectory, scratch)
		if err != nil {
			return nil, err
		}
		arguments := []string{"-test.run", "^TestLemmaRoutes$", "-test.count", "1"}
		for position, index := range group {
			results[index] = runEntry(binary, arguments, position, entries[index].text, entries[index].directory, scratch)
		}
	}
	return results, nil
}

// byPackage groups entry indexes by package, in the order first seen.
func byPackage(entries []entry, indexes []int) [][]int {
	var order []string
	groups := map[string][]int{}
	for _, index := range indexes {
		path := entries[index].importPath
		if _, seen := groups[path]; !seen {
			order = append(order, path)
		}
		groups[path] = append(groups[path], index)
	}
	var grouped [][]int
	for _, path := range order {
		grouped = append(grouped, groups[path])
	}
	return grouped
}

// buildMainProgram writes a main package at the module root that imports
// every entry's package, builds it, and removes it.
func buildMainProgram(entries []entry, indexes []int, known knownSet, moduleDirectory, scratch string) (string, error) {
	directory := filepath.Join(moduleDirectory, fmt.Sprintf("%s%d", generatedPrefix, os.Getpid()))
	if err := os.Mkdir(directory, 0o755); err != nil {
		return "", err
	}
	defer os.RemoveAll(directory)
	var imports, table strings.Builder
	qualifiers := map[string]string{}
	isNamed := map[string]bool{}
	for _, index := range indexes {
		if entries[index].symbol != "" {
			isNamed[entries[index].importPath] = true
		}
	}
	for _, index := range indexes {
		e := entries[index]
		qualifier, seen := qualifiers[e.importPath]
		if !seen {
			qualifier = fmt.Sprintf("lemmatarget%d", len(qualifiers))
			qualifiers[e.importPath] = qualifier
			// A package named only for its init functions is imported blank.
			alias := qualifier
			if !isNamed[e.importPath] {
				alias = "_"
			}
			fmt.Fprintf(&imports, "\t%s %q\n", alias, e.importPath)
		}
		fmt.Fprintf(&table, "\t{%q, func() any { return %s }},\n", e.text, e.reference(qualifier))
	}
	source := generatedHeader("main", imports.String(), known) + fmt.Sprintf(`
var lemmaEntries = []struct {
	name  string
	value func() any
}{
%s}

func main() {
	position, _ := lemmastrconv.Atoi(lemmaos.Getenv("LEMMA_GO_ENTRY_INDEX"))
	lemmaRead(lemmaEntries[position].name, lemmaEntries[position].value)
	lemmaos.Exit(0)
}
`, table.String()) + walkerSource + known.source()
	if err := os.WriteFile(filepath.Join(directory, "main.go"), []byte(source), 0o644); err != nil {
		return "", err
	}
	binary := filepath.Join(scratch, "routes")
	_, err := goCommand(moduleDirectory, buildEnvironment(), "build", "-tags", "lemma,lemma_routes", "-o", binary, "./"+filepath.Base(directory))
	if err != nil {
		return "", fmt.Errorf("could not build the program reading %s: %w", entryList(entries, indexes), err)
	}
	return binary, nil
}

// buildTestProgram writes a test file into one package, builds the
// package's test binary, and removes the file.
func buildTestProgram(entries []entry, indexes []int, known knownSet, moduleDirectory, scratch string) (string, error) {
	first := entries[indexes[0]]
	file := filepath.Join(first.directory, fmt.Sprintf("%s%d_test.go", generatedPrefix, os.Getpid()))
	var table strings.Builder
	for _, index := range indexes {
		e := entries[index]
		fmt.Fprintf(&table, "\t{%q, func() any { return %s }},\n", e.text, e.reference(""))
	}
	source := generatedHeader(first.name, "\tlemmatesting \"testing\"\n", known) + fmt.Sprintf(`
var lemmaEntries = []struct {
	name  string
	value func() any
}{
%s}

func TestLemmaRoutes(t *lemmatesting.T) {
	position, _ := lemmastrconv.Atoi(lemmaos.Getenv("LEMMA_GO_ENTRY_INDEX"))
	lemmaRead(lemmaEntries[position].name, lemmaEntries[position].value)
}
`, table.String()) + walkerSource + known.source()
	if err := os.WriteFile(file, []byte(source), 0o644); err != nil {
		return "", err
	}
	defer os.Remove(file)
	binary := filepath.Join(scratch, fmt.Sprintf("routes-%d.test", indexes[0]))
	_, err := goCommand(moduleDirectory, buildEnvironment(), "test", "-c", "-tags", "lemma,lemma_routes", "-o", binary, first.importPath)
	if err != nil {
		return "", fmt.Errorf("could not build the test reading %s: %w", entryList(entries, indexes), err)
	}
	return binary, nil
}

// runEntry runs one entry, first with nil pointers, then, if that call
// failed, with pointers to zero values. A call that fails both ways gives
// what the fuller attempt found, and both reasons.
func runEntry(binary string, arguments []string, position int, name, directory, scratch string) found {
	first := runOnce(binary, arguments, position, "nil", name, directory, scratch)
	if !first.Failed {
		return first
	}
	second := runOnce(binary, arguments, position, "allocated", name, directory, scratch)
	if !second.Failed {
		fmt.Fprintf(os.Stderr, "lemma: %s was read with pointers to zero values; with nil pointers it failed\n", name)
		return second
	}
	fuller, other := second, first
	if len(first.Routes) > len(second.Routes) {
		fuller, other = first, second
	}
	fuller.Unread = append(fuller.Unread, failureReasons(other)...)
	return fuller
}

// failureReasons is the unread modules of a failed attempt that name the
// entry, not the routes it found.
func failureReasons(attempt found) []json.RawMessage {
	var reasons []json.RawMessage
	for _, raw := range attempt.Unread {
		if strings.Contains(string(raw), "(called with") {
			reasons = append(reasons, raw)
		}
	}
	return reasons
}

// runOnce runs one entry in one zero-value mode and reads its result.
func runOnce(binary string, arguments []string, position int, zeroMode, name, directory, scratch string) found {
	resultPath := filepath.Join(scratch, fmt.Sprintf("result-%d-%s.json", position, zeroMode))
	command := exec.Command(binary, arguments...)
	command.Dir = directory
	command.Env = append(os.Environ(),
		"LEMMA_GO_ENTRY_INDEX="+strconv.Itoa(position),
		"LEMMA_GO_RESULT="+resultPath,
		"LEMMA_GO_ZERO="+zeroMode,
	)
	// The service's own logging goes to stderr, never to the document.
	command.Stdout = os.Stderr
	command.Stderr = os.Stderr
	runError := command.Run()
	text, readError := os.ReadFile(resultPath)
	if readError != nil {
		reason := "exited before reporting its routes"
		if runError != nil {
			reason += ": " + runError.Error()
		}
		return found{Unread: []json.RawMessage{unreadModule(name, reason)}, Failed: true}
	}
	var result found
	if err := json.Unmarshal(text, &result); err != nil {
		return found{Unread: []json.RawMessage{unreadModule(name, "reported routes that could not be read: "+err.Error())}}
	}
	for index, entry := range result.Unread {
		result.Unread[index] = withZeroMode(entry, name, zeroMode)
	}
	return result
}

// withZeroMode notes in an unread module's reason which zero values the
// call had.
func withZeroMode(raw json.RawMessage, name, zeroMode string) json.RawMessage {
	var fields map[string]any
	if json.Unmarshal(raw, &fields) != nil || fields["module"] != name {
		return raw
	}
	description := "with nil pointers"
	if zeroMode == "allocated" {
		description = "with pointers to zero values"
	}
	fields["reason"] = fmt.Sprintf("%v (called %s)", fields["reason"], description)
	text, _ := json.Marshal(fields)
	return text
}

func unreadModule(module, reason string) json.RawMessage {
	text, _ := json.Marshal(map[string]string{"module": module, "reason": reason})
	return text
}

func unreadRoute(method, path, reason string) json.RawMessage {
	text, _ := json.Marshal(map[string]string{"method": method, "path": path, "reason": reason})
	return text
}

// buildDocument turns what every entry found into the OpenAPI document.
func buildDocument(entries []entry, results []found, root string) (map[string]any, int) {
	treatSubtreesAsLeaves := os.Getenv("LEMMA_GO_SUBTREES") == "leaves"
	sources := newFunctionSources(entries)
	unread := []json.RawMessage{}
	var operations []operation
	seen := map[string]bool{}
	for index, result := range results {
		unread = append(unread, result.Unread...)
		for _, route := range result.Routes {
			method, path, isSubtree := routeShape(route)
			if isSubtree && !treatSubtreesAsLeaves {
				reason := fmt.Sprintf("%s hands every path under %s to a handler whose routes cannot be listed", entries[index].text, path)
				unread = append(unread, unreadModule(path, reason))
				continue
			}
			methods := route.Methods
			if method != "" {
				methods = []string{method}
			}
			site, fn := handlerSite(route, sources, root)
			if len(methods) == 0 {
				for _, any := range []string{"get", "post", "put", "patch", "delete"} {
					unread = append(unread, unreadRoute(any, path, "the route serves every method"))
				}
				continue
			}
			for _, method := range describedMethods(methods) {
				key := method + " " + canonicalKey(path)
				if seen[key] {
					continue
				}
				seen[key] = true
				operations = append(operations, operation{method: method, path: path, site: site, fn: fn})
			}
		}
	}
	dropWrapperSites(operations)
	paths := map[string]map[string]any{}
	for _, op := range operations {
		item := paths[op.path]
		if item == nil {
			item = map[string]any{}
			paths[op.path] = item
		}
		described := map[string]any{"responses": map[string]any{"default": map[string]any{"description": "routes only"}}}
		if parameters := pathParameters(op.path); len(parameters) > 0 {
			described["parameters"] = parameters
		}
		if op.handler != "" {
			described["x-lemma-handler"] = op.handler
		}
		item[op.method] = described
	}
	unread = withRelativePaths(unread, root)
	sortUnread(unread)
	document := map[string]any{
		"openapi":          "3.0.3",
		"info":             map[string]any{"title": "generated by Lemma", "version": "0"},
		"paths":            paths,
		"x-lemma-schemas":  false,
		"x-lemma-unread":   unread,
		"x-lemma-complete": !hasUnreadModule(unread),
	}
	return document, len(operations)
}

// routeShape is a route's method (when its pattern names one), its path in
// OpenAPI's form, and whether it serves a whole subtree.
func routeShape(route foundRoute) (string, string, bool) {
	if route.Source == "mux" {
		return "", openAPIPath(route.Pattern), route.IsSubtree
	}
	// ServeMux: "[METHOD ][HOST]/PATH".
	pattern := strings.TrimSpace(route.Pattern)
	method := ""
	if before, after, found := strings.Cut(pattern, " "); found && !strings.Contains(before, "/") {
		method, pattern = strings.ToLower(before), strings.TrimSpace(after)
	}
	if slash := strings.Index(pattern, "/"); slash > 0 {
		pattern = pattern[slash:]
	}
	isSubtree := false
	switch {
	case strings.HasSuffix(pattern, "/{$}"):
		pattern = strings.TrimSuffix(pattern, "{$}")
	case strings.HasSuffix(pattern, "...}"):
		isSubtree = true
		pattern = pattern[:strings.LastIndex(pattern, "{")]
	case strings.HasSuffix(pattern, "/"):
		isSubtree = true
	}
	return method, openAPIPath(pattern), isSubtree
}

// openAPIPath turns router variables into OpenAPI's: `{id:[0-9]+}` (mux)
// and `{path...}` (ServeMux) become `{id}` and `{path}`.
func openAPIPath(template string) string {
	var out strings.Builder
	depth := 0
	inName := false
	for _, character := range template {
		switch {
		case character == '{':
			depth++
			if depth == 1 {
				inName = true
				out.WriteRune(character)
			}
		case character == '}':
			depth--
			if depth == 0 {
				inName = false
				out.WriteRune(character)
			}
		case depth == 0:
			out.WriteRune(character)
		case depth == 1 && inName && character == ':':
			inName = false
		case depth == 1 && inName:
			out.WriteRune(character)
		}
	}
	return strings.ReplaceAll(out.String(), "...}", "}")
}

// describedMethods lowers methods and leaves out HEAD beside GET and
// OPTIONS beside anything else: routers add them for every route, and
// documents do not list them.
func describedMethods(methods []string) []string {
	set := map[string]bool{}
	for _, method := range methods {
		set[strings.ToLower(method)] = true
	}
	if set["get"] {
		delete(set, "head")
	}
	if len(set) > 1 {
		delete(set, "options")
	}
	var lowered []string
	for method := range set {
		lowered = append(lowered, method)
	}
	sort.Strings(lowered)
	return lowered
}

// canonicalKey is a path with its parameter names erased and no trailing
// slash, so two spellings of one route are one operation.
func canonicalKey(path string) string {
	var out strings.Builder
	inParameter := false
	for _, character := range path {
		switch {
		case character == '{':
			inParameter = true
			out.WriteString("{}")
		case character == '}':
			inParameter = false
		case !inParameter:
			out.WriteRune(character)
		}
	}
	key := strings.TrimSuffix(out.String(), "/")
	if key == "" {
		return "/"
	}
	return key
}

func pathParameters(path string) []any {
	var parameters []any
	for _, part := range strings.Split(path, "/") {
		if !strings.HasPrefix(part, "{") || !strings.HasSuffix(part, "}") {
			continue
		}
		parameters = append(parameters, map[string]any{
			"name": part[1 : len(part)-1], "in": "path", "required": true,
			"schema": map[string]any{"type": "string"},
		})
	}
	return parameters
}

// handlerSite is where a route is handled, `file:line` relative to `root`:
// the handler's definition, found in the source when the runtime only
// knows a generated wrapper (a method value), or else the line that
// registered the route.
func handlerSite(route foundRoute, sources *functionSources, root string) (string, string) {
	file, line := route.File, route.Line
	if file == "" || file == "<autogenerated>" || strings.HasSuffix(route.Function, "-fm") || relativeTo(root, file) == "" {
		if resolvedFile, resolvedLine, ok := sources.find(route.Function); ok {
			file, line = resolvedFile, resolvedLine
		}
	}
	if relative := relativeTo(root, file); relative != "" && line > 0 {
		return fmt.Sprintf("%s:%d", relative, line), route.Function
	}
	if registeredFile, registeredLine, ok := splitSite(route.Registered); ok {
		if relative := relativeTo(root, registeredFile); relative != "" {
			return fmt.Sprintf("%s:%d", relative, registeredLine), route.Function
		}
	}
	return "", route.Function
}

// dropWrapperSites gives each operation its handler site, unless the site
// is a closure several paths share: that is a wrapper's (a middleware's)
// rather than a handler's, and placing every route there would be wrong, so
// those routes are left unplaced. A named function serving several paths
// is a handler and keeps them.
func dropWrapperSites(operations []operation) {
	paths := map[string]map[string]bool{}
	for _, op := range operations {
		if op.site == "" || !isClosure(op.fn) {
			continue
		}
		if paths[op.site] == nil {
			paths[op.site] = map[string]bool{}
		}
		paths[op.site][canonicalKey(op.path)] = true
	}
	for index := range operations {
		site := operations[index].site
		if site != "" && len(paths[site]) < sharedClosurePaths {
			operations[index].handler = site
		}
	}
}

// functionSources finds a function's definition by its runtime name, for
// handlers the runtime only knows as a wrapper.
type functionSources struct {
	directories map[string]string // import path -> directory
	parsed      map[string][]*ast.File
	files       *token.FileSet
}

func newFunctionSources(entries []entry) *functionSources {
	sources := &functionSources{directories: map[string]string{}, parsed: map[string][]*ast.File{}, files: token.NewFileSet()}
	for _, e := range entries {
		sources.directories[e.importPath] = e.directory
	}
	return sources
}

// find locates `example.com/svc/api.(*Server).list-fm` and the like.
func (s *functionSources) find(runtimeName string) (string, int, bool) {
	importPath, typeName, functionName, ok := splitRuntimeName(runtimeName)
	if !ok {
		return "", 0, false
	}
	directory, known := s.directories[importPath]
	if !known {
		text, err := goCommand(".", nil, "list", "-e", "-f", "{{.Dir}}", importPath)
		directory = strings.TrimSpace(text)
		if err != nil {
			directory = ""
		}
		s.directories[importPath] = directory
	}
	if directory == "" {
		return "", 0, false
	}
	for _, file := range s.parse(directory) {
		for _, declaration := range file.Decls {
			function, isFunction := declaration.(*ast.FuncDecl)
			if !isFunction || function.Name.Name != functionName || receiverName(function) != typeName {
				continue
			}
			position := s.files.Position(function.Pos())
			return position.Filename, position.Line, true
		}
	}
	return "", 0, false
}

func (s *functionSources) parse(directory string) []*ast.File {
	if files, done := s.parsed[directory]; done {
		return files
	}
	var files []*ast.File
	names, _ := filepath.Glob(filepath.Join(directory, "*.go"))
	sort.Strings(names)
	for _, name := range names {
		if strings.HasSuffix(name, "_test.go") {
			continue
		}
		file, err := parser.ParseFile(s.files, name, nil, parser.SkipObjectResolution)
		if err == nil {
			files = append(files, file)
		}
	}
	s.parsed[directory] = files
	return files
}

// splitRuntimeName splits `path/pkg.(*T).M-fm`, `path/pkg.T.M` and
// `path/pkg.F` into import path, receiver type ("" for none) and name.
// Closures (`F.func1`) are not definitions and give false.
func splitRuntimeName(name string) (string, string, string, bool) {
	name = strings.TrimSuffix(name, "-fm")
	slash := strings.LastIndex(name, "/")
	dot := strings.Index(name[slash+1:], ".")
	if dot < 0 {
		return "", "", "", false
	}
	importPath, rest := name[:slash+1+dot], name[slash+2+dot:]
	parts := strings.Split(rest, ".")
	for _, part := range parts {
		if strings.HasPrefix(part, "func") && strings.TrimLeft(part[4:], "0123456789") == "" && len(part) > 4 {
			return "", "", "", false
		}
	}
	switch len(parts) {
	case 1:
		return importPath, "", parts[0], true
	case 2:
		receiver := strings.Trim(parts[0], "(*)")
		if bracket := strings.Index(receiver, "["); bracket >= 0 {
			receiver = receiver[:bracket]
		}
		return importPath, receiver, parts[1], true
	}
	return "", "", "", false
}

func receiverName(function *ast.FuncDecl) string {
	if function.Recv == nil || len(function.Recv.List) == 0 {
		return ""
	}
	expression := function.Recv.List[0].Type
	for {
		switch typed := expression.(type) {
		case *ast.StarExpr:
			expression = typed.X
		case *ast.IndexExpr:
			expression = typed.X
		case *ast.IndexListExpr:
			expression = typed.X
		case *ast.Ident:
			return typed.Name
		default:
			return ""
		}
	}
}

// isClosure reports whether a runtime function name is a function literal's
// (`pkg.F.func1`, `pkg.F.func1.2`).
func isClosure(runtimeName string) bool {
	afterPackage := runtimeName[strings.LastIndex(runtimeName, "/")+1:]
	for _, part := range strings.Split(afterPackage, ".")[1:] {
		if strings.HasPrefix(part, "func") || strings.TrimLeft(part, "0123456789") == "" {
			return true
		}
	}
	return false
}

func hasUnreadModule(unread []json.RawMessage) bool {
	for _, raw := range unread {
		var fields map[string]any
		if json.Unmarshal(raw, &fields) == nil {
			if _, isModule := fields["module"]; isModule {
				return true
			}
		}
	}
	return false
}

// withRelativePaths writes files in reasons (where a constructor panicked)
// relative to `root`, so a document does not depend on where the code was
// checked out.
func withRelativePaths(unread []json.RawMessage, root string) []json.RawMessage {
	prefix, _ := json.Marshal(root + string(filepath.Separator))
	quoted := strings.Trim(string(prefix), "\"")
	for index, raw := range unread {
		unread[index] = json.RawMessage(strings.ReplaceAll(string(raw), quoted, ""))
	}
	return unread
}

func sortUnread(unread []json.RawMessage) {
	sort.SliceStable(unread, func(a, b int) bool { return string(unread[a]) < string(unread[b]) })
}

// keepModuleFiles saves go.mod and go.sum and returns what puts them back:
// building the generated program must never change the code being read.
func keepModuleFiles(moduleDirectory string) func() {
	saved := map[string][]byte{}
	for _, name := range []string{"go.mod", "go.sum"} {
		path := filepath.Join(moduleDirectory, name)
		if text, err := os.ReadFile(path); err == nil {
			saved[path] = text
		}
	}
	return func() {
		for path, text := range saved {
			if current, err := os.ReadFile(path); err != nil || !bytes.Equal(current, text) {
				_ = os.WriteFile(path, text, 0o644)
			}
		}
	}
}

// removeStaleCopies removes generated files a killed run left in the
// module, those whose process is gone.
func removeStaleCopies(moduleDirectory string) {
	_ = filepath.WalkDir(moduleDirectory, func(path string, item os.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		name := item.Name()
		if item.IsDir() && path != moduleDirectory && (strings.HasPrefix(name, ".") || name == "vendor" || name == "node_modules") {
			return filepath.SkipDir
		}
		if !strings.HasPrefix(name, generatedPrefix) {
			return nil
		}
		digits := strings.TrimSuffix(strings.TrimPrefix(name, generatedPrefix), "_test.go")
		pid, convertError := strconv.Atoi(digits)
		if convertError != nil || isRunning(pid) {
			return nil
		}
		_ = os.RemoveAll(path)
		if item.IsDir() {
			return filepath.SkipDir
		}
		return nil
	})
}

func isRunning(pid int) bool {
	if pid <= 0 {
		return false
	}
	err := syscall.Kill(pid, 0)
	return err == nil || errors.Is(err, syscall.EPERM)
}

// buildEnvironment is the environment builds run with: GOFLAGS without
// -trimpath, which would leave handler files without the paths that place
// them.
func buildEnvironment() []string {
	var flags []string
	for _, flag := range strings.Fields(os.Getenv("GOFLAGS")) {
		if flag != "-trimpath" && flag != "-trimpath=true" {
			flags = append(flags, flag)
		}
	}
	return []string{"GOFLAGS=" + strings.Join(flags, " ")}
}

// goCommand runs `go` in `directory`; its error carries go's stderr.
func goCommand(directory string, environment []string, arguments ...string) (string, error) {
	command := exec.Command("go", arguments...)
	command.Dir = directory
	command.Env = append(os.Environ(), environment...)
	var stdout, stderr bytes.Buffer
	command.Stdout = &stdout
	command.Stderr = &stderr
	if err := command.Run(); err != nil {
		return stdout.String(), fmt.Errorf("go %s: %v\n%s", strings.Join(arguments, " "), err, strings.TrimSpace(stderr.String()))
	}
	return stdout.String(), nil
}

// knownSet is which logging modules the module requires, so the generated
// program can hand their discarding loggers to constructors.
type knownSet struct{ zap, logrus, zerolog, mux bool }

// knownTypes reads the module's requirements from go.mod without loading
// anything, so nothing is resolved or downloaded.
func knownTypes(moduleDirectory string) knownSet {
	text, err := goCommand(moduleDirectory, nil, "mod", "edit", "-json")
	if err != nil {
		return knownSet{}
	}
	var module struct{ Require []struct{ Path string } }
	if json.Unmarshal([]byte(text), &module) != nil {
		return knownSet{}
	}
	var set knownSet
	for _, requirement := range module.Require {
		switch requirement.Path {
		case "go.uber.org/zap":
			set.zap = true
		case "github.com/sirupsen/logrus":
			set.logrus = true
		case "github.com/rs/zerolog":
			set.zerolog = true
		case "github.com/gorilla/mux":
			set.mux = true
		}
	}
	return set
}

func (set knownSet) imports() string {
	var imports strings.Builder
	if set.zap {
		imports.WriteString("\tlemmazap \"go.uber.org/zap\"\n")
	}
	if set.logrus {
		imports.WriteString("\tlemmalogrus \"github.com/sirupsen/logrus\"\n")
	}
	if set.zerolog {
		imports.WriteString("\tlemmazerolog \"github.com/rs/zerolog\"\n")
	}
	if set.mux {
		imports.WriteString("\tlemmamux \"github.com/gorilla/mux\"\n")
	}
	return imports.String()
}

// source is the generated code that depends on what the module requires.
func (set knownSet) source() string {
	var cases strings.Builder
	if set.zap {
		cases.WriteString(`	case lemmareflect.TypeOf((*lemmazap.Logger)(nil)):
		return lemmareflect.ValueOf(lemmazap.NewNop()), true
	case lemmareflect.TypeOf((*lemmazap.SugaredLogger)(nil)):
		return lemmareflect.ValueOf(lemmazap.NewNop().Sugar()), true
`)
	}
	if set.logrus {
		cases.WriteString(`	case lemmareflect.TypeOf((*lemmalogrus.Logger)(nil)):
		logger := lemmalogrus.New()
		logger.SetOutput(lemmaio.Discard)
		return lemmareflect.ValueOf(logger), true
	case lemmareflect.TypeOf((*lemmalogrus.Entry)(nil)):
		logger := lemmalogrus.New()
		logger.SetOutput(lemmaio.Discard)
		return lemmareflect.ValueOf(lemmalogrus.NewEntry(logger)), true
`)
	}
	if set.zerolog {
		cases.WriteString(`	case lemmareflect.TypeOf(lemmazerolog.Logger{}):
		return lemmareflect.ValueOf(lemmazerolog.Nop()), true
	case lemmareflect.TypeOf((*lemmazerolog.Logger)(nil)):
		logger := lemmazerolog.Nop()
		return lemmareflect.ValueOf(&logger), true
`)
	}
	known := `
func lemmaKnown(t lemmareflect.Type) (lemmareflect.Value, bool) {
	switch t {
	case lemmareflect.TypeOf((*lemmacontext.Context)(nil)).Elem():
		return lemmareflect.ValueOf(lemmacontext.Background()), true
	case lemmareflect.TypeOf((*lemmalog.Logger)(nil)):
		return lemmareflect.ValueOf(lemmalog.New(lemmaio.Discard, "", 0)), true
	case lemmareflect.TypeOf((*lemmaslog.Logger)(nil)):
		return lemmareflect.ValueOf(lemmaslog.New(lemmaslog.NewTextHandler(lemmaio.Discard, nil))), true
` + cases.String() + `	}
	return lemmareflect.Value{}, false
}
`
	if set.mux {
		return known + muxSource
	}
	return known + noMuxSource
}

func generatedHeader(packageName, extraImports string, known knownSet) string {
	return fmt.Sprintf(`//go:build lemma_routes

// Code generated by Lemma's go-http-routes adapter to read this module's
// routes; removed after reading. DO NOT EDIT.

package %s

import (
	lemmacontext "context"
	lemmajson "encoding/json"
	lemmafmt "fmt"
	lemmaio "io"
	lemmalog "log"
	lemmaslog "log/slog"
	lemmahttp "net/http"
	lemmaos "os"
	lemmareflect "reflect"
	lemmaruntime "runtime"
	lemmastrconv "strconv"
	lemmastrings "strings"
	lemmatime "time"
	lemmaunsafe "unsafe"
%s%s)
`, packageName, extraImports, known.imports())
}

// walkerSource is the generated program's reading: call the entry, find
// the routers, list their routes. Every name starts with lemma so it
// cannot meet a name of the package it is written into.
const walkerSource = `
type lemmaRoute struct {
	Source     string   ` + "`json:\"source\"`" + `
	Pattern    string   ` + "`json:\"pattern\"`" + `
	Methods    []string ` + "`json:\"methods\"`" + `
	Subtree    bool     ` + "`json:\"subtree\"`" + `
	Function   string   ` + "`json:\"function\"`" + `
	File       string   ` + "`json:\"file\"`" + `
	Line       int      ` + "`json:\"line\"`" + `
	Registered string   ` + "`json:\"registered\"`" + `
}

type lemmaReading struct {
	routes  []lemmaRoute
	unread  []map[string]string
	routers []lemmareflect.Value
	walked  map[uintptr]bool
	failed  bool
}

func lemmaRead(name string, target func() any) {
	reading := &lemmaReading{walked: map[uintptr]bool{}}
	allocate := lemmaos.Getenv("LEMMA_GO_ZERO") == "allocated"
	var results []lemmareflect.Value
	var fresh []lemmareflect.Value
	done := make(chan struct{})
	go func() {
		defer close(done)
		defer func() {
			if recovered := recover(); recovered != nil {
				reading.failed = true
				reading.unread = append(reading.unread, map[string]string{
					"module": name,
					"reason": lemmafmt.Sprintf("panicked: %v%s", recovered, lemmaPanicSite()),
				})
			}
		}()
		value := target()
		if value == nil {
			return
		}
		function := lemmareflect.ValueOf(value)
		if function.Kind() != lemmareflect.Func {
			results = []lemmareflect.Value{function}
			return
		}
		var arguments []lemmareflect.Value
		arguments, fresh = lemmaArguments(function.Type(), allocate)
		results = function.Call(arguments)
	}()
	timeoutMs, _ := lemmastrconv.Atoi(lemmaos.Getenv("LEMMA_GO_CALL_TIMEOUT_MS"))
	if timeoutMs <= 0 {
		timeoutMs = 60000
	}
	select {
	case <-done:
		for _, value := range append(fresh, results...) {
			lemmaFind(value, 0, reading)
		}
	case <-lemmatime.After(lemmatime.Duration(timeoutMs) * lemmatime.Millisecond):
		reading.failed = true
		reading.unread = append(reading.unread, map[string]string{
			"module": name,
			"reason": lemmafmt.Sprintf("did not return within %d ms", timeoutMs),
		})
	}
	// A package alone, or a function that registers on http.DefaultServeMux
	// and returns no router, is read through http.DefaultServeMux.
	if len(reading.routers) == 0 {
		lemmaFind(lemmareflect.ValueOf(lemmahttp.DefaultServeMux), 0, reading)
	}
	for index := 0; index < len(reading.routers); index++ {
		lemmaWalk(reading.routers[index], reading)
	}
	text, _ := lemmajson.Marshal(map[string]any{"routes": reading.routes, "unread": reading.unread, "failed": reading.failed})
	_ = lemmaos.WriteFile(lemmaos.Getenv("LEMMA_GO_RESULT"), text, 0o644)
}

// lemmaArguments is a zero value for each parameter, a fresh router for a
// router parameter (returned again so it is read after the call).
func lemmaArguments(function lemmareflect.Type, allocate bool) ([]lemmareflect.Value, []lemmareflect.Value) {
	count := function.NumIn()
	if function.IsVariadic() {
		count--
	}
	var arguments, fresh []lemmareflect.Value
	for index := 0; index < count; index++ {
		parameter := function.In(index)
		if router, ok := lemmaFreshRouter(parameter); ok {
			arguments = append(arguments, router)
			fresh = append(fresh, router)
			continue
		}
		arguments = append(arguments, lemmaZero(parameter, allocate, 0))
	}
	return arguments, fresh
}

func lemmaFreshRouter(t lemmareflect.Type) (lemmareflect.Value, bool) {
	if t == lemmareflect.TypeOf((*lemmahttp.ServeMux)(nil)) {
		return lemmareflect.ValueOf(lemmahttp.NewServeMux()), true
	}
	return lemmaFreshMux(t)
}

// lemmaZero is t's zero value, with discarding loggers and a background
// context in place of nil, and, when allocate is set, pointers to zero
// values in place of nil pointers.
func lemmaZero(t lemmareflect.Type, allocate bool, depth int) lemmareflect.Value {
	if known, ok := lemmaKnown(t); ok {
		return known
	}
	if depth > 4 {
		return lemmareflect.Zero(t)
	}
	switch t.Kind() {
	case lemmareflect.Pointer:
		if !allocate {
			return lemmareflect.Zero(t)
		}
		pointer := lemmareflect.New(t.Elem())
		pointer.Elem().Set(lemmaZero(t.Elem(), allocate, depth+1))
		return pointer
	case lemmareflect.Struct:
		value := lemmareflect.New(t).Elem()
		for index := 0; index < t.NumField(); index++ {
			if !t.Field(index).IsExported() {
				continue
			}
			value.Field(index).Set(lemmaZero(t.Field(index).Type, allocate, depth+1))
		}
		return value
	case lemmareflect.Map:
		if allocate {
			return lemmareflect.MakeMap(t)
		}
	}
	return lemmareflect.Zero(t)
}

// lemmaFind collects the routers in value: the value itself, or what a
// struct holds, exported or not, a few levels down.
func lemmaFind(value lemmareflect.Value, depth int, reading *lemmaReading) {
	if !value.IsValid() || depth > 4 {
		return
	}
	switch value.Kind() {
	case lemmareflect.Interface:
		if !value.IsNil() {
			lemmaFind(value.Elem(), depth, reading)
		}
	case lemmareflect.Pointer:
		if value.IsNil() {
			return
		}
		usable := lemmareflect.NewAt(value.Type().Elem(), value.UnsafePointer())
		if lemmaIsRouter(usable.Type()) {
			reading.routers = append(reading.routers, usable)
			return
		}
		lemmaFind(usable.Elem(), depth+1, reading)
	case lemmareflect.Struct:
		for index := 0; index < value.NumField(); index++ {
			lemmaFind(value.Field(index), depth+1, reading)
		}
	}
}

func lemmaIsRouter(t lemmareflect.Type) bool {
	return t == lemmareflect.TypeOf((*lemmahttp.ServeMux)(nil)) || lemmaIsMux(t)
}

// lemmaWalk lists one router's routes, once per router.
func lemmaWalk(router lemmareflect.Value, reading *lemmaReading) {
	if reading.walked[router.Pointer()] {
		return
	}
	reading.walked[router.Pointer()] = true
	if serveMux, ok := router.Interface().(*lemmahttp.ServeMux); ok {
		lemmaWalkServeMux(serveMux, reading)
		return
	}
	lemmaWalkMux(router, reading)
}

// lemmaWalkServeMux reads a ServeMux's registrations by reflection, since
// it has no API listing them: every struct holding a pattern and a handler
// (routingNode since Go 1.22, muxEntry before).
func lemmaWalkServeMux(serveMux *lemmahttp.ServeMux, reading *lemmaReading) {
	lemmaVisit(lemmareflect.ValueOf(serveMux).Elem(), map[uintptr]bool{}, reading)
}

func lemmaVisit(value lemmareflect.Value, seen map[uintptr]bool, reading *lemmaReading) {
	switch value.Kind() {
	case lemmareflect.Pointer:
		if value.IsNil() || seen[value.Pointer()] {
			return
		}
		seen[value.Pointer()] = true
		lemmaVisit(value.Elem(), seen, reading)
	case lemmareflect.Struct:
		lemmaRecordServeMuxEntry(value, reading)
		for index := 0; index < value.NumField(); index++ {
			if value.Field(index).Kind() != lemmareflect.Interface {
				lemmaVisit(value.Field(index), seen, reading)
			}
		}
	case lemmareflect.Map:
		entries := value.MapRange()
		for entries.Next() {
			lemmaVisit(entries.Value(), seen, reading)
		}
	case lemmareflect.Slice, lemmareflect.Array:
		for index := 0; index < value.Len(); index++ {
			lemmaVisit(value.Index(index), seen, reading)
		}
	}
}

func lemmaRecordServeMuxEntry(value lemmareflect.Value, reading *lemmaReading) {
	pattern := value.FieldByName("pattern")
	handler := value.FieldByName("handler")
	if !handler.IsValid() {
		handler = value.FieldByName("h")
	}
	if !pattern.IsValid() || !handler.IsValid() || handler.Kind() != lemmareflect.Interface || handler.IsNil() {
		return
	}
	text, location := "", ""
	switch pattern.Kind() {
	case lemmareflect.String:
		text = pattern.String()
	case lemmareflect.Pointer:
		if pattern.IsNil() {
			return
		}
		text = pattern.Elem().FieldByName("str").String()
		if loc := pattern.Elem().FieldByName("loc"); loc.IsValid() {
			location = loc.String()
		}
	}
	if text == "" {
		return
	}
	if lemmaQueueNested(handler, reading) {
		return
	}
	file, line, function := lemmaHandlerSite(handler)
	reading.routes = append(reading.routes, lemmaRoute{
		Source: "servemux", Pattern: text, Function: function, File: file, Line: line, Registered: location,
	})
}

// lemmaQueueNested reads a router mounted as another's handler as a router
// of its own; its patterns already carry their full paths.
func lemmaQueueNested(handler lemmareflect.Value, reading *lemmaReading) bool {
	for handler.Kind() == lemmareflect.Interface && !handler.IsNil() {
		handler = handler.Elem()
	}
	if handler.Kind() != lemmareflect.Pointer || handler.IsNil() {
		return false
	}
	usable := lemmareflect.NewAt(handler.Type().Elem(), handler.UnsafePointer())
	if !lemmaIsRouter(usable.Type()) {
		return false
	}
	reading.routers = append(reading.routers, usable)
	return true
}

// lemmaHandlerSite is the file, line and name of the function handling a
// route: the function itself, or a handler type's ServeHTTP.
func lemmaHandlerSite(handler lemmareflect.Value) (string, int, string) {
	for handler.Kind() == lemmareflect.Interface {
		if handler.IsNil() {
			return "", 0, ""
		}
		handler = handler.Elem()
	}
	var pc uintptr
	if handler.Kind() == lemmareflect.Func {
		if handler.IsNil() {
			return "", 0, ""
		}
		pc = handler.Pointer()
	} else {
		method, ok := handler.Type().MethodByName("ServeHTTP")
		if !ok {
			return "", 0, ""
		}
		pc = method.Func.Pointer()
	}
	function := lemmaruntime.FuncForPC(pc)
	if function == nil {
		return "", 0, ""
	}
	file, line := function.FileLine(function.Entry())
	return file, line, function.Name()
}

// lemmaPanicSite is the innermost frame of a panic outside the runtime and
// this file, to say where the constructor failed.
func lemmaPanicSite() string {
	pcs := make([]uintptr, 64)
	frames := lemmaruntime.CallersFrames(pcs[:lemmaruntime.Callers(3, pcs)])
	for {
		frame, more := frames.Next()
		generated := lemmastrings.Contains(frame.File, "lemma_routes_")
		if !generated && !lemmastrings.HasPrefix(frame.Function, "runtime.") && !lemmastrings.HasPrefix(frame.Function, "reflect.") && frame.File != "" {
			return lemmafmt.Sprintf(" at %s:%d in %s", frame.File, frame.Line, frame.Function)
		}
		if !more {
			return ""
		}
	}
}

var _ = lemmaunsafe.Pointer(nil)
`

// muxSource reads gorilla/mux routers through Router.Walk.
const muxSource = `
func lemmaFreshMux(t lemmareflect.Type) (lemmareflect.Value, bool) {
	if t == lemmareflect.TypeOf((*lemmamux.Router)(nil)) {
		return lemmareflect.ValueOf(lemmamux.NewRouter()), true
	}
	return lemmareflect.Value{}, false
}

func lemmaIsMux(t lemmareflect.Type) bool {
	return t == lemmareflect.TypeOf((*lemmamux.Router)(nil))
}

func lemmaWalkMux(router lemmareflect.Value, reading *lemmaReading) {
	muxRouter, ok := router.Interface().(*lemmamux.Router)
	if !ok {
		return
	}
	_ = muxRouter.Walk(func(route *lemmamux.Route, _ *lemmamux.Router, _ []*lemmamux.Route) error {
		handler := route.GetHandler()
		if handler == nil {
			return nil
		}
		handlerValue := lemmareflect.ValueOf(&handler).Elem()
		if lemmaQueueNested(handlerValue, reading) {
			return nil
		}
		template, err := route.GetPathTemplate()
		if err != nil {
			_, _, function := lemmaHandlerSite(handlerValue)
			reading.unread = append(reading.unread, map[string]string{
				"module": "route handled by " + function,
				"reason": "the route has no path template (it is matched by host or other matchers)",
			})
			return nil
		}
		pathRegexp, _ := route.GetPathRegexp()
		methods, _ := route.GetMethods()
		file, line, function := lemmaHandlerSite(handlerValue)
		reading.routes = append(reading.routes, lemmaRoute{
			Source: "mux", Pattern: template, Methods: methods,
			Subtree: !lemmastrings.HasSuffix(pathRegexp, "$"),
			Function: function, File: file, Line: line,
		})
		return nil
	})
}
`

// noMuxSource stands in for muxSource in a module without gorilla/mux.
const noMuxSource = `
func lemmaFreshMux(lemmareflect.Type) (lemmareflect.Value, bool) { return lemmareflect.Value{}, false }

func lemmaIsMux(lemmareflect.Type) bool { return false }

func lemmaWalkMux(lemmareflect.Value, *lemmaReading) {}
`

func entryList(entries []entry, indexes []int) string {
	var names []string
	for _, index := range indexes {
		names = append(names, entries[index].text)
	}
	return strings.Join(names, ", ")
}

// splitSite splits `file:line`.
func splitSite(site string) (string, int, bool) {
	colon := strings.LastIndex(site, ":")
	if colon < 0 {
		return "", 0, false
	}
	line, err := strconv.Atoi(site[colon+1:])
	if err != nil {
		return "", 0, false
	}
	return site[:colon], line, true
}

// relativeTo is `file` relative to `root`, or "" when it is outside it.
func relativeTo(root, file string) string {
	if file == "" || !filepath.IsAbs(file) {
		return ""
	}
	relative, err := filepath.Rel(root, realPath(file))
	if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return ""
	}
	return filepath.ToSlash(relative)
}

func realPath(path string) string {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return path
	}
	if resolved, err := filepath.EvalSymlinks(absolute); err == nil {
		return resolved
	}
	return absolute
}

func nonEmptyLines(text string) []string {
	var lines []string
	for _, line := range strings.Split(text, "\n") {
		if trimmed := strings.TrimSpace(line); trimmed != "" {
			lines = append(lines, trimmed)
		}
	}
	return lines
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}

func isExported(name string) bool {
	for _, character := range name {
		return unicode.IsUpper(character)
	}
	return false
}
