//! Type-checks TypeScript declaration files via the TypeScript compiler API embedded in QuickJS.

use quick_js::Context;
use serde::Deserialize;

const TYPESCRIPT_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/typescript.js"));
const LIB_ES5_DTS: &str = include_str!(concat!(env!("OUT_DIR"), "/lib.es5.d.ts"));

/// Preamble: shim the Node.js globals TypeScript.js expects before loading it.
const PREAMBLE: &str = r#"
var process = {
    env: {},
    argv: [],
    platform: 'linux',
    version: 'v18.0.0',
    versions: {},
    cwd: function() { return '/'; },
    exit: function(code) { throw new Error('process.exit(' + (code || 0) + ')'); }
};

var console = {
    log: function() {},
    error: function() {},
    warn: function() {},
    info: function() {},
    debug: function() {}
};

var setTimeout = function(fn, delay) { return 0; };
var clearTimeout = function(id) {};
var setInterval = function(fn, delay) { return 0; };
var clearInterval = function(id) {};

var module = { exports: {} };
var exports = module.exports;

function require(id) {
    if (id === 'perf_hooks') {
        return {
            performance: {
                now: function() { return 0; },
                mark: function() {},
                measure: function() {}
            },
            PerformanceObserver: function() {}
        };
    }
    if (id === 'path') {
        var sep = '/';
        var delimiter = ':';
        function normalizeSlashes(p) { return String(p).replace(/\\/g, '/'); }
        function normalize(p) {
            p = normalizeSlashes(p);
            var parts = p.split('/');
            var result = [];
            for (var i = 0; i < parts.length; i++) {
                var part = parts[i];
                if (part === '..') {
                    if (result.length > 0 && result[result.length - 1] !== '..') {
                        result.pop();
                    } else {
                        result.push('..');
                    }
                } else if (part !== '.') {
                    result.push(part);
                }
            }
            var out = result.join('/');
            if (p.charAt(0) === '/') out = '/' + out;
            return out || '.';
        }
        function isAbsolute(p) { return String(p).charAt(0) === '/'; }
        function join() {
            var parts = [];
            for (var i = 0; i < arguments.length; i++) parts.push(String(arguments[i]));
            return normalize(parts.join('/'));
        }
        function resolve() {
            var resolved = '';
            for (var i = arguments.length - 1; i >= 0; i--) {
                var p = String(arguments[i]);
                if (isAbsolute(p)) { resolved = p; break; }
                resolved = p + '/' + resolved;
            }
            if (!isAbsolute(resolved)) resolved = '/' + resolved;
            return normalize(resolved);
        }
        function dirname(p) {
            p = normalizeSlashes(p);
            var idx = p.lastIndexOf('/');
            if (idx < 0) return '.';
            if (idx === 0) return '/';
            return p.slice(0, idx);
        }
        function basename(p, ext) {
            p = normalizeSlashes(p);
            var idx = p.lastIndexOf('/');
            var base = idx >= 0 ? p.slice(idx + 1) : p;
            if (ext && base.slice(-ext.length) === ext) base = base.slice(0, -ext.length);
            return base;
        }
        function extname(p) {
            p = normalizeSlashes(p);
            var base = basename(p);
            var idx = base.lastIndexOf('.');
            if (idx <= 0) return '';
            return base.slice(idx);
        }
        function relative(from, to) {
            from = resolve(from).split('/');
            to = resolve(to).split('/');
            while (from.length && to.length && from[0] === to[0]) {
                from.shift(); to.shift();
            }
            var up = from.map(function() { return '..'; });
            return up.concat(to).join('/') || '.';
        }
        return {
            sep: sep,
            delimiter: delimiter,
            join: join,
            resolve: resolve,
            dirname: dirname,
            basename: basename,
            extname: extname,
            relative: relative,
            normalize: normalize,
            isAbsolute: isAbsolute
        };
    }
    if (id === 'os') {
        return {
            EOL: '\n',
            platform: function() { return 'linux'; },
            homedir: function() { return '/home/user'; },
            tmpdir: function() { return '/tmp'; },
            hostname: function() { return 'localhost'; },
            cpus: function() { return []; },
            type: function() { return 'Linux'; }
        };
    }
    if (id === 'crypto') {
        return {
            createHash: function(alg) {
                return {
                    update: function(data) { return this; },
                    digest: function(enc) { return ''; }
                };
            },
            randomBytes: function(n) { return new Uint8Array(n); }
        };
    }
    if (id === 'inspector') {
        return { open: function() {}, close: function() {}, url: function() { return null; } };
    }
    if (id === 'tty') {
        return {
            isatty: function() { return false; },
            ReadStream: function() {},
            WriteStream: function() {}
        };
    }
    if (id === 'buffer') {
        return { Buffer: { from: function() { return []; }, alloc: function(n) { return new Uint8Array(n); } } };
    }
    if (id === 'fs') {
        return {
            readFileSync: function() { throw new Error('fs not available'); },
            existsSync: function() { return false; },
            statSync: function() { throw new Error('fs not available'); }
        };
    }
    // Unknown modules return an empty object rather than throwing so TypeScript.js
    // optional requires don't crash the runtime.
    return {};
}
"#;

/// Postamble: captures `ts` from `module.exports` and defines `checkDts`.
const POSTAMBLE: &str = r#"
var ts = module.exports;

exports.checkDts = function(source, libEs5Source) {
    var files = {
        'input.d.ts': source,
        'lib.es5.d.ts': libEs5Source
    };

    var host = {
        fileExists: function(fileName) {
            return Object.prototype.hasOwnProperty.call(files, fileName);
        },
        readFile: function(fileName) {
            return files[fileName];
        },
        getSourceFile: function(fileName, languageVersion) {
            var src = files[fileName];
            if (src === undefined) return undefined;
            return ts.createSourceFile(fileName, src, languageVersion);
        },
        getDefaultLibFileName: function(options) {
            return 'lib.es5.d.ts';
        },
        writeFile: function() {},
        getCurrentDirectory: function() { return '/'; },
        getDirectories: function() { return []; },
        getCanonicalFileName: function(f) { return f; },
        useCaseSensitiveFileNames: function() { return true; },
        getNewLine: function() { return '\n'; }
    };

    var options = {
        strict: true,
        noEmit: true,
        skipDefaultLibCheck: true,
        declaration: true,
        noResolve: true
    };

    var program = ts.createProgram(['input.d.ts'], options, host);
    var rawDiags = ts.getPreEmitDiagnostics(program);

    var result = [];
    rawDiags.forEach(function(d) {
        if (!d.file || d.file.fileName !== 'input.d.ts') return;
        var msg = ts.flattenDiagnosticMessageText(d.messageText, '\n');
        var start = null;
        var end = null;
        if (d.start !== undefined && d.start !== null) {
            var startPos = d.file.getLineAndCharacterOfPosition(d.start);
            start = { line: startPos.line + 1, column: startPos.character };
        }
        if (d.start !== undefined && d.start !== null && d.length !== undefined && d.length !== null) {
            var endPos = d.file.getLineAndCharacterOfPosition(d.start + d.length);
            end = { line: endPos.line + 1, column: endPos.character };
        }
        result.push({ message: msg, start: start, end: end });
    });

    return JSON.stringify(result);
};
"#;

/// A source location within a `.d.ts` file.
#[derive(Debug, Deserialize, PartialEq)]
pub struct Position {
    /// 1-based line number.
    pub line: u32,
    /// 0-based column number.
    pub column: u32,
}

/// A single diagnostic emitted by the TypeScript compiler.
#[derive(Debug, Deserialize)]
pub struct Diagnostic {
    /// Human-readable error message.
    pub message: String,
    /// Start position of the offending span, if available.
    pub start: Option<Position>,
    /// End position of the offending span, if available.
    pub end: Option<Position>,
}

/// TypeScript checker error.
#[derive(Debug)]
pub enum Error {
    /// QuickJS runtime error.
    Runtime(String),
    /// Failed to deserialize the diagnostics JSON.
    Deserialize(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(msg) => write!(f, "runtime error: {msg}"),
            Self::Deserialize(msg) => write!(f, "deserialization error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// A reusable TypeScript type-checker backed by QuickJS.
///
/// Create once, check many `.d.ts` files. The QuickJS context (with the full
/// TypeScript compiler loaded) is reused across calls.
pub struct TypeScriptChecker {
    ctx: Context,
}

impl TypeScriptChecker {
    /// Create a new checker instance.
    ///
    /// Loads the TypeScript compiler into a QuickJS context. This is slow on
    /// the first call because the full `typescript.js` bundle is parsed and
    /// evaluated. Subsequent `diagnostics` calls are fast.
    pub fn new() -> Result<Self, Error> {
        let ctx = Context::new().map_err(|e| Error::Runtime(e.to_string()))?;

        ctx.eval(PREAMBLE)
            .map_err(|e| Error::Runtime(format!("preamble: {e}")))?;

        ctx.eval(TYPESCRIPT_JS)
            .map_err(|e| Error::Runtime(format!("typescript.js: {e}")))?;

        ctx.eval(POSTAMBLE)
            .map_err(|e| Error::Runtime(format!("postamble: {e}")))?;

        Ok(Self { ctx })
    }

    /// Check a `.d.ts` source string for type errors.
    ///
    /// Returns an empty `Vec` when the source is valid. Returns diagnostics
    /// (each with a message and optional source location) when there are errors.
    /// Returns `Err` only on runtime or deserialization failure.
    pub fn diagnostics(&self, source: &str) -> Result<Vec<Diagnostic>, Error> {
        let source_json =
            serde_json::to_string(source).map_err(|e| Error::Runtime(e.to_string()))?;
        let lib_json =
            serde_json::to_string(LIB_ES5_DTS).map_err(|e| Error::Runtime(e.to_string()))?;

        let script =
            format!("(function() {{ return exports.checkDts({source_json}, {lib_json}); }})()");

        let json: String = self
            .ctx
            .eval_as(&script)
            .map_err(|e| Error::Runtime(e.to_string()))?;

        serde_json::from_str(&json).map_err(|e| Error::Deserialize(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_valid_declarations() {
        // Arrange
        let checker = TypeScriptChecker::new().expect("failed to create checker");

        // Act
        let diags = checker
            .diagnostics("export type Foo = string;")
            .expect("runtime error");

        // Assert
        assert!(
            diags.is_empty(),
            "valid .d.ts should produce no diagnostics"
        );
    }

    #[test]
    fn checker_catches_type_error() {
        // Arrange
        let checker = TypeScriptChecker::new().expect("failed to create checker");

        // Act
        let diags = checker
            .diagnostics("export declare const x: ThisTypeDoesNotExistXYZ123;")
            .expect("runtime error");

        // Assert
        assert!(
            !diags.is_empty(),
            "invalid .d.ts should produce diagnostics"
        );
        let first = &diags[0];
        assert!(
            !first.message.is_empty(),
            "diagnostic should have a message"
        );
        let start = first
            .start
            .as_ref()
            .expect("diagnostic should have start position");
        assert!(start.line >= 1, "start line should be >= 1");
    }

    #[test]
    fn checker_reusable() {
        // Arrange
        let checker = TypeScriptChecker::new().expect("failed to create checker");

        // Act and Assert
        let first = checker
            .diagnostics("export type A = string;")
            .expect("first check runtime error");
        assert!(
            first.is_empty(),
            "first valid file should have no diagnostics"
        );

        let second = checker
            .diagnostics("export type B = number;")
            .expect("second check runtime error");
        assert!(
            second.is_empty(),
            "second valid file should have no diagnostics"
        );
    }
}
