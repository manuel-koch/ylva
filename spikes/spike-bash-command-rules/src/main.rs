//! Spike: empirically confirm `tree-sitter-bash`'s AST shapes for the
//! constructs that `src/sandbox/bash_command_extractor.rs` (the real,
//! tested extractor) is built around.
//!
//! This is a **spike, not production code** — a throwaway probe whose
//! findings are recorded as doc comments in the real extractor module and
//! re-verified there by its own `grammar_assumptions` test module. Run with
//! `cargo run` from this crate's directory.

use tree_sitter::Parser;

/// Print a script's full parse tree, including anonymous/unnamed nodes
/// (`;`, `&`, quote characters, etc.) and field names, so terminator/
/// backgrounding/field-name behaviour is directly observable.
fn print_tree(source: &str) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("load bash grammar");
    let tree = parser.parse(source, None).expect("parse");
    println!(
        "=== {source:?} === root_has_error={}",
        tree.root_node().has_error()
    );
    let mut cursor = tree.walk();
    let mut depth = 0usize;
    loop {
        let node = cursor.node();
        let field = cursor.field_name().unwrap_or("");
        println!(
            "{}[{}] {} named={} err={} [{}..{}] {:?}",
            "  ".repeat(depth),
            field,
            node.kind(),
            node.is_named(),
            node.is_error(),
            node.start_byte(),
            node.end_byte(),
            &source[node.start_byte()..node.end_byte()]
        );
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            depth -= 1;
        }
    }
}

fn main() {
    // --- terminators: `;`, `&`, and a trailing `&` ---
    print_tree("cmd1 ; cmd2");
    print_tree("cmd1 & cmd2");
    print_tree("cmd &");
    print_tree("cmd1 &\ncmd2");

    // --- `list` (`&&`/`||`) vs. `pipeline` (`|`) ---
    print_tree("cmd1 && cmd2 || cmd3");
    print_tree("a | b | c");

    // --- leading variable assignment ---
    print_tree("FOO=bar git status");

    // --- newline-separated statements (no dedicated node) ---
    print_tree("git commit -m \"fix bug\"\ngit push");

    // --- quoting: unquoted glob, quoted glob-like text, brace expansion ---
    print_tree("cat *.txt");
    print_tree("cat \"*.txt\"");
    print_tree("cat '*.txt'");
    print_tree("echo {a,b}");
    print_tree("echo \"{a,b}\"");
    print_tree("echo \"a\"\"b\"");

    // --- out-of-scope constructs: subshell, substitution, expansion,
    // control flow, function definition, heredoc, redirect ---
    print_tree("(cmd1)");
    print_tree("echo $(cmd1)");
    print_tree("echo `cmd1`");
    print_tree("echo $FOO");
    print_tree("echo \"$FOO\"");
    print_tree("if true; then echo hi; fi");
    print_tree("for x in a b; do echo $x; done");
    print_tree("func() { echo hi; }");
    print_tree("cat <<EOF\nhi\nEOF");
    print_tree("cmd > file.txt");

    // --- parse failure ---
    print_tree("cmd1 |");
}
