use std::ffi::OsString;
use std::io::{self, Write};

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<OsString>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let interrupt_handler = match sai_driver::InterruptHandler::install() {
        Ok(handler) => handler,
        Err(error) => {
            let _ = writeln!(stderr, "sai: cannot install interrupt handler: {error}");
            std::process::exit(4);
        }
    };
    let cancellation = interrupt_handler.cancellation_token();
    let exit_code =
        sai_cli::cli::run_with_cancellation(&arguments, &cancellation, &mut stdout, &mut stderr);
    std::process::exit(exit_code);
}
