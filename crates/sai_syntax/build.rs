#[path = "build_support.rs"]
mod build_support;

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=sai.ungram");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=src/ast.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support.rs");

    let grammar = fs::read_to_string("sai.ungram").expect("read sai.ungram");
    let syntax_source = fs::read_to_string("src/lib.rs").expect("read SyntaxKind declaration");
    let ast_source = fs::read_to_string("src/ast.rs").expect("read existing AST declarations");
    let existing_types = build_support::existing_ast_types(&ast_source);
    let nodes = build_support::parse_nodes(&grammar, &existing_types);
    build_support::validate_node_kinds(&nodes, &syntax_source);

    let generated = build_support::generate_nodes(&nodes);
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("ast_nodes.rs");
    fs::write(output, generated).expect("write generated AST node wrappers");
}
