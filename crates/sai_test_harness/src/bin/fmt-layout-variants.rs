#![forbid(unsafe_code)]

use std::io::{self, Read, Write};

use sai_test_harness::generate_layout_variants;

const VARIANT_COUNT: usize = 16;

fn main() {
    if let Err(error) = run() {
        eprintln!("fmt-layout-variants: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let test_path = arguments
        .next()
        .ok_or("expected one test path relative to tests/")?;
    if arguments.next().is_some() {
        return Err("expected one test path relative to tests/".into());
    }
    let test_path = test_path
        .into_string()
        .map_err(|_| "test path is not valid UTF-8")?;

    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let variants = generate_layout_variants(&test_path, &input, VARIANT_COUNT)?;
    let variants = variants
        .into_iter()
        .map(String::from_utf8)
        .collect::<Result<Vec<_>, _>>()?;

    let mut stdout = io::BufWriter::new(io::stdout().lock());
    serde_json::to_writer(&mut stdout, &variants)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
