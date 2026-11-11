//! Signing confinement: the contract must never be drivable as a signing
//! oracle.
//!
//! The Chain Signatures contract signs whatever 32-byte hash the contract asks
//! it to, for the contract's own key. So the only way funds on that key can move
//! is through code that asks it to sign, and every such request must be built
//! from a transaction the contract constructed and policy approved. This test
//! scans the production source for every dispatch to the signer's `sign` and
//! fails unless each one sits inside a function on the allowed list below.
//!
//! It scans `src/` as source, not behaviour, and does not touch production
//! code. It reads every non-test `.rs` file, tokenizes it (so comments, doc
//! comments and unrelated strings are ignored, and `#[cfg(test)]` items and
//! modules are skipped), tracks which function each token is inside, and flags
//! any occurrence of the identifier `sign` or the string literal `"sign"`.
//!
//! This is a tripwire against an accidental or careless second dispatch, not a
//! proof against deliberate obfuscation (building the name at runtime, say);
//! the method-surface snapshot and review cover the rest.

use std::fs;
use std::path::{Path, PathBuf};

/// The only functions that may dispatch to the signer's `sign`, as
/// `(file under the crate root, function name)`.
///
/// To allow another one, add a line here after reviewing it. `owner_evm_withdraw`
/// joins this list when it lands:
/// `("src/gate.rs", "owner_evm_withdraw")`.
const ALLOWED_SIGN_DISPATCH: &[(&str, &str)] = &[("src/gate.rs", "request_evm_payment")];

// ---- A small Rust tokenizer ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: usize,
}

fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let (mut i, mut line) = (0, 1);
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '/' if at(i + 1) == '/' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if at(i + 1) == '*' => {
                let mut depth = 1;
                i += 2;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '/' && at(i + 1) == '*' {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && at(i + 1) == '/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        if chars[i] == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                }
            }
            '"' => {
                let (text, next, lines) = string_literal(&chars, i + 1);
                tokens.push(Token {
                    tok: Tok::Str(text),
                    line,
                });
                line += lines;
                i = next;
            }
            '\'' => {
                // A character literal, or a lifetime.
                if at(i + 1) == '\\' {
                    i += 2;
                    while i < chars.len() && chars[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                } else if at(i + 2) == '\'' {
                    i += 3;
                } else {
                    i += 1;
                    while at(i).is_alphanumeric() || at(i) == '_' {
                        i += 1;
                    }
                }
            }
            c if c.is_ascii_digit() => {
                while at(i).is_alphanumeric() || at(i) == '_' {
                    i += 1;
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                // A raw or byte string literal starts with an identifier-looking prefix.
                if let Some((text, next, lines)) = prefixed_string(&chars, i) {
                    tokens.push(Token {
                        tok: Tok::Str(text),
                        line,
                    });
                    line += lines;
                    i = next;
                    continue;
                }
                let start = i;
                while at(i).is_alphanumeric() || at(i) == '_' {
                    i += 1;
                }
                tokens.push(Token {
                    tok: Tok::Ident(chars[start..i].iter().collect()),
                    line,
                });
            }
            c => {
                tokens.push(Token {
                    tok: Tok::Punct(c),
                    line,
                });
                i += 1;
            }
        }
    }
    tokens
}

/// The body of a `"..."` literal starting just after the opening quote.
/// Returns its text, the index after the closing quote, and lines consumed.
fn string_literal(chars: &[char], mut i: usize) -> (String, usize, usize) {
    let (mut text, mut lines) = (String::new(), 0);
    while i < chars.len() && chars[i] != '"' {
        if chars[i] == '\\' && i + 1 < chars.len() {
            text.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if chars[i] == '\n' {
            lines += 1;
        }
        text.push(chars[i]);
        i += 1;
    }
    (text, i + 1, lines)
}

/// `b"..."`, `r"..."`, `r#"..."#`, `br"..."` and friends, or `None` if the
/// identifier at `i` is not such a prefix.
fn prefixed_string(chars: &[char], i: usize) -> Option<(String, usize, usize)> {
    let at = |j: usize| chars.get(j).copied().unwrap_or('\0');
    let mut j = i;
    if at(j) == 'b' {
        j += 1;
    }
    let raw = at(j) == 'r';
    if raw {
        j += 1;
    }
    if j == i {
        return None;
    }
    if !raw {
        return (at(j) == '"').then(|| string_literal(chars, j + 1));
    }
    let hashes = (j..).take_while(|&k| at(k) == '#').count();
    let open = j + hashes;
    if at(open) != '"' {
        return None; // a raw identifier such as `r#type`, or a plain identifier
    }
    let closing: Vec<char> = std::iter::once('"')
        .chain(std::iter::repeat_n('#', hashes))
        .collect();
    let mut k = open + 1;
    let (mut text, mut lines) = (String::new(), 0);
    while k < chars.len() && chars[k..].get(..closing.len()) != Some(&closing[..]) {
        if chars[k] == '\n' {
            lines += 1;
        }
        text.push(chars[k]);
        k += 1;
    }
    Some((text, k + closing.len(), lines))
}

// ---- The scan ----

#[derive(Debug, PartialEq)]
struct Hit {
    line: usize,
    /// The function the occurrence is inside, if any.
    function: Option<String>,
    what: String,
}

#[derive(Debug, Default)]
struct Scan {
    hits: Vec<Hit>,
    /// Modules declared `#[cfg(test)] mod name;`, whose files are test-only.
    test_modules: Vec<String>,
}

enum Frame {
    Function(String),
    Block,
}

fn ident(tokens: &[Token], at: usize) -> Option<&str> {
    match tokens.get(at).map(|t| &t.tok) {
        Some(Tok::Ident(name)) => Some(name),
        _ => None,
    }
}

fn punct(tokens: &[Token], at: usize) -> Option<char> {
    match tokens.get(at).map(|t| &t.tok) {
        Some(Tok::Punct(c)) => Some(*c),
        _ => None,
    }
}

/// Whether the attribute starting at `#` at `start` is a test-only `cfg`.
/// Returns the index after the attribute and whether it is test-only.
fn attribute(tokens: &[Token], start: usize) -> (usize, bool) {
    let mut i = start + 1;
    if punct(tokens, i) == Some('!') {
        i += 1;
    }
    if punct(tokens, i) != Some('[') {
        return (start + 1, false);
    }
    let (mut depth, begin) = (0, i);
    loop {
        match punct(tokens, i) {
            Some('[') => depth += 1,
            Some(']') => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ if i >= tokens.len() => break,
            _ => {}
        }
        i += 1;
    }
    let inner = &tokens[begin + 1..i.min(tokens.len())];
    let names: Vec<&str> = inner
        .iter()
        .filter_map(|t| match &t.tok {
            Tok::Ident(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let test_only =
        names.first() == Some(&"cfg") && names.contains(&"test") && !names.contains(&"not");
    (i + 1, test_only)
}

/// The index after the item starting at `start`: through its `;`, or through its
/// balanced `{ ... }` body. Also returns the module name if it is `mod name;`.
fn skip_item(tokens: &[Token], start: usize) -> (usize, Option<String>) {
    let mut depth = 0i32;
    let mut i = start;
    while i < tokens.len() {
        match punct(tokens, i) {
            Some('(') | Some('[') => depth += 1,
            Some(')') | Some(']') => depth -= 1,
            Some(';') if depth <= 0 => {
                let module = (ident(tokens, start) == Some("mod"))
                    .then(|| ident(tokens, start + 1).map(str::to_string))
                    .flatten();
                return (i + 1, module);
            }
            Some('{') if depth <= 0 => {
                let mut braces = 0;
                while i < tokens.len() {
                    match punct(tokens, i) {
                        Some('{') => braces += 1,
                        Some('}') => {
                            braces -= 1;
                            if braces == 0 {
                                return (i + 1, None);
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                return (i, None);
            }
            _ => {}
        }
        i += 1;
    }
    (i, None)
}

fn scan(source: &str) -> Scan {
    let tokens = tokenize(source);
    let mut result = Scan::default();
    let mut frames: Vec<Frame> = Vec::new();
    // A `fn name` waiting for its body `{`, with the bracket depth it was seen at.
    let mut pending_fn: Option<(String, i32)> = None;
    let mut depth = 0i32;
    let mut skip_next_item = false;
    let mut i = 0;

    while i < tokens.len() {
        // `#[cfg(test)]` marks the next item as test-only: skip all of it.
        if punct(&tokens, i) == Some('#') {
            let (next, test_only) = attribute(&tokens, i);
            skip_next_item |= test_only;
            i = next;
            continue;
        }
        if skip_next_item {
            skip_next_item = false;
            let (next, module) = skip_item(&tokens, i);
            result.test_modules.extend(module);
            i = next;
            continue;
        }

        let function = || {
            frames.iter().rev().find_map(|frame| match frame {
                Frame::Function(name) => Some(name.clone()),
                Frame::Block => None,
            })
        };
        match &tokens[i].tok {
            Tok::Ident(name) if name == "sign" => result.hits.push(Hit {
                line: tokens[i].line,
                function: function(),
                what: "the identifier `sign`".to_string(),
            }),
            Tok::Str(text) if text == "sign" => result.hits.push(Hit {
                line: tokens[i].line,
                function: function(),
                what: "the string literal \"sign\"".to_string(),
            }),
            Tok::Ident(name) if name == "fn" => {
                if let Some(function_name) = ident(&tokens, i + 1) {
                    pending_fn = Some((function_name.to_string(), depth));
                }
            }
            Tok::Punct('(') | Tok::Punct('[') => depth += 1,
            Tok::Punct(')') | Tok::Punct(']') => depth -= 1,
            Tok::Punct(';') => {
                if pending_fn.as_ref().is_some_and(|(_, at)| *at == depth) {
                    pending_fn = None; // a declaration without a body
                }
            }
            Tok::Punct('{') => match pending_fn.take() {
                Some((name, at)) if at == depth => frames.push(Frame::Function(name)),
                other => {
                    pending_fn = other;
                    frames.push(Frame::Block);
                }
            },
            Tok::Punct('}') => {
                frames.pop();
            }
            _ => {}
        }
        i += 1;
    }
    result
}

// ---- Scanning the crate ----

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Every production-source occurrence of a signing dispatch, as
/// `(relative file, hit)`, skipping files that are only compiled for tests.
fn production_hits() -> (Vec<(String, Hit)>, Vec<String>) {
    let root = crate_root();
    let lib_modules =
        scan(&fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs")).test_modules;

    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    let mut hits = Vec::new();
    let mut scanned = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(&root)
            .expect("under root")
            .to_string_lossy()
            .replace('\\', "/");
        let inside_test_module = lib_modules.iter().any(|module| {
            relative == format!("src/{module}.rs")
                || relative.starts_with(&format!("src/{module}/"))
        });
        if inside_test_module {
            continue;
        }
        scanned.push(relative.clone());
        for hit in scan(&fs::read_to_string(&path).expect("read source")).hits {
            hits.push((relative.clone(), hit));
        }
    }
    (hits, scanned)
}

fn allowed(file: &str, function: &Option<String>) -> bool {
    function
        .as_deref()
        .is_some_and(|name| ALLOWED_SIGN_DISPATCH.contains(&(file, name)))
}

// ---- Tests ----

#[test]
fn signing_dispatch_is_confined_to_the_allowed_functions() {
    let (hits, scanned) = production_hits();
    assert!(
        scanned.contains(&"src/gate.rs".to_string()) && scanned.contains(&"src/lib.rs".to_string()),
        "the scan did not read the production sources: {scanned:?}"
    );

    let stray: Vec<String> = hits
        .iter()
        .filter(|(file, hit)| !allowed(file, &hit.function))
        .map(|(file, hit)| {
            format!(
                "  {file}:{} in {} ({})",
                hit.line,
                hit.function
                    .as_deref()
                    .map_or("no function".to_string(), |name| format!("fn {name}")),
                hit.what
            )
        })
        .collect();
    assert!(
        stray.is_empty(),
        "a signing dispatch sits outside the allowed functions:\n{}\n\n\
         Only these functions may ask the signer to sign: {ALLOWED_SIGN_DISPATCH:?}.\n\
         Any other dispatch lets the contract be driven as a signing oracle. If a new \
         signing entry point is deliberate, review it (it must build the transaction \
         itself and evaluate policy) and add it to ALLOWED_SIGN_DISPATCH.",
        stray.join("\n")
    );

    // Not vacuous, and no stale allowance: every allowed function really
    // contains a dispatch, so an entry cannot linger and silently permit one
    // later.
    for (file, function) in ALLOWED_SIGN_DISPATCH {
        assert!(
            hits.iter().any(
                |(hit_file, hit)| hit_file == file && hit.function.as_deref() == Some(function)
            ),
            "ALLOWED_SIGN_DISPATCH lists {function} in {file}, but it contains no signing dispatch"
        );
    }
}

#[test]
fn the_scan_skips_test_only_modules_of_the_real_crate() {
    let root = crate_root();
    let modules = scan(&fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs")).test_modules;
    for expected in ["gate_tests", "model_tests", "test_support"] {
        assert!(
            modules.contains(&expected.to_string()),
            "{expected} not recognized as test-only: {modules:?}"
        );
    }
    let (_, scanned) = production_hits();
    assert!(!scanned
        .iter()
        .any(|file| file.contains("model_tests") || file.ends_with("gate_tests.rs")));
}

// ---- The scanner, on synthetic sources ----

fn functions(source: &str) -> Vec<(Option<String>, usize)> {
    scan(source)
        .hits
        .into_iter()
        .map(|h| (h.function, h.line))
        .collect()
}

#[test]
fn a_dispatch_in_a_function_is_attributed_to_it() {
    let source = "
impl Minder {
    pub fn request_evm_payment(&mut self) {
        let call = Promise::new(id).function_call(\"sign\".to_string(), args, 1, gas);
        let closure = |x: u8| { let _ = \"sign\"; };
    }
    pub fn unrelated(&self) {
        let _ = self.helper().sign(1);
    }
}";
    assert_eq!(
        functions(source),
        vec![
            (Some("request_evm_payment".to_string()), 4),
            (Some("request_evm_payment".to_string()), 5),
            (Some("unrelated".to_string()), 8),
        ]
    );
}

#[test]
fn every_way_of_naming_the_signer_method_is_flagged() {
    // A string literal (plain, byte, raw), a method call, a path call, a
    // declaration and a bare identifier.
    for (source, what) in [
        ("fn f() { x(\"sign\"); }", "plain literal"),
        ("fn f() { x(b\"sign\"); }", "byte literal"),
        ("fn f() { x(r\"sign\"); }", "raw literal"),
        ("fn f() { x(r#\"sign\"#); }", "hashed raw literal"),
        ("fn f() { a.sign(1); }", "method call"),
        ("fn f() { Signer::sign(1); }", "path call"),
        ("trait T { fn sign(&self); }", "declaration"),
        ("fn f() { let sign = 1; }", "binding"),
        (
            "const METHOD: &str = \"sign\";",
            "constant outside any function",
        ),
    ] {
        assert_eq!(
            scan(source).hits.len(),
            1,
            "{what} was not flagged exactly once"
        );
    }
}

#[test]
fn a_dispatch_outside_any_function_has_no_function() {
    assert_eq!(functions("const M: &str = \"sign\";"), vec![(None, 1)]);
    assert_eq!(
        functions("trait T {\n    fn sign(&self);\n}"),
        vec![(None, 2)]
    );
}

#[test]
fn comments_docs_and_lookalikes_are_ignored() {
    let source = "
// sign here
/// Asks the signer to sign.
/* a block comment with sign /* nested sign */ still sign */
fn f() {
    let a = \"signing_hash\";
    let b = \"design\";
    let c = \"please sign\";
    let signature = 1; let signer = 2; let signed = 3; let assign = 4;
    let d = r#\"a raw \"sign\" inside a longer string\"#;
}";
    assert!(scan(source).hits.is_empty(), "{:?}", scan(source).hits);
}

#[test]
fn braces_in_strings_chars_and_lifetimes_do_not_confuse_attribution() {
    let source = "
fn outer<'a>(x: &'a str) -> &'a str {
    let open = '{';
    let close = '}';
    let text = \"} } {\";
    let tab = '\\t';
    let _ = \"sign\";
    x
}
fn after() { let _ = \"sign\"; }";
    assert_eq!(
        functions(source),
        vec![
            (Some("outer".to_string()), 7),
            (Some("after".to_string()), 10)
        ]
    );
}

#[test]
fn test_only_items_are_skipped_and_test_modules_are_reported() {
    let source = "
fn real() { let _ = \"sign\"; }
#[cfg(test)]
mod tests {
    fn fake() { let _ = \"sign\"; }
}
#[cfg(test)]
mod helper_tests;
#[cfg(test)]
fn only_in_tests() { let _ = \"sign\"; }
#[cfg(all(test, feature = \"x\"))]
fn also_only_in_tests() { let _ = \"sign\"; }
#[cfg(not(test))]
fn not_a_test() { let _ = \"sign\"; }
#[cfg(feature = \"test-helpers\")]
fn helper_feature() { let _ = \"sign\"; }
fn last() { let _ = \"sign\"; }";
    let scan = scan(source);
    let functions: Vec<_> = scan
        .hits
        .iter()
        .map(|h| h.function.clone().unwrap())
        .collect();
    assert_eq!(functions, ["real", "not_a_test", "helper_feature", "last"]);
    assert_eq!(scan.test_modules, ["helper_tests"]);
}

#[test]
fn only_the_listed_function_in_the_listed_file_is_allowed() {
    let name = |s: &str| Some(s.to_string());
    assert!(allowed("src/gate.rs", &name("request_evm_payment")));
    assert!(!allowed("src/gate.rs", &name("freeze")));
    assert!(
        !allowed("src/lib.rs", &name("request_evm_payment")),
        "same name, wrong file"
    );
    assert!(
        !allowed("src/gate.rs", &None),
        "outside any function is never allowed"
    );
}
