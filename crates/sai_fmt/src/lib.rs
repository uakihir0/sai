#![forbid(unsafe_code)]

mod printer;

pub use printer::{Doc, GroupId, PrintError, print, print_with_forced_groups};
