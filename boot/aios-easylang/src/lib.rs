//! EasyLang Core: a small imperative language for AIOS WebAssembly blocks.
//!
//! The language targets exactly the subset [`aios_wasmhost`] can validate and
//! run, so a block written in EasyLang needs no toolchain beyond this crate:
//!
//! ```text
//! # hello.easy
//! log "hello from aios-wasmhost"
//! ```
//!
//! [`compile`] turns the source into a complete module with one exported
//! `run` function, one linear memory page when the program logs text, and only
//! the host imports the program actually uses (`aios.log`, `aios.exit`,
//! `aios.now_ms`, `aios.yield`).
//!
//! The crate is `#[no_std]` + `alloc` and has no host dependency, so the same
//! compiler can run inside the kernel; the `aios-easylang` binary is the host
//! tool that packs `*.easy` files into the image.

#![no_std]

extern crate alloc;

mod ast;
mod codegen;
mod error;
mod lexer;
mod parser;

pub use error::Error;

/// Compiles EasyLang source into a runnable WebAssembly block.
///
/// The returned bytes form a complete module: one exported `run` function with
/// no parameters and no result, the imports the program uses, and at most one
/// linear memory page holding the string literals.
pub fn compile(source: &str) -> Result<alloc::vec::Vec<u8>, Error> {
    let tokens = lexer::tokenize(source)?;
    let program = parser::parse(tokens)?;
    codegen::compile(&program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_wasmhost::{Env, Host, Module, Rights, Token, Trap, DEFAULT_FUEL, PAGE_SIZE};
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    struct Log {
        lines: Vec<String>,
    }

    impl Env for Log {
        fn log(&mut self, text: &str) {
            self.lines.push(String::from(text));
        }

        fn now_ms(&mut self) -> i32 {
            7
        }
    }

    fn run(source: &str, rights: Rights) -> (Result<Option<i32>, Trap>, Vec<String>, u32) {
        let bytes = compile(source).unwrap();
        let module = Module::parse(&bytes).unwrap();
        let mut memory = vec![0u8; PAGE_SIZE as usize];
        let mut host = Host::new(
            &mut memory,
            Log { lines: Vec::new() },
            Token::new(0, rights, DEFAULT_FUEL),
        );
        host.instantiate(&module).unwrap();
        let result = host.call(&module, "run", &[]);
        let lines = host.env().lines.clone();
        let fuel = DEFAULT_FUEL - host.fuel();
        (result, lines, fuel)
    }

    #[test]
    fn logs_text() {
        let (result, lines, _) = run("log \"hello\"\nlog \"world\"\n", Rights::LOG);
        assert_eq!(result, Ok(None));
        assert_eq!(lines, vec![String::from("hello"), String::from("world")]);
    }

    #[test]
    fn computes_and_exits() {
        let source = "\
let sum = 0
let i = 5
while i > 0 {
    sum = sum + i
    i = i - 1
}
exit sum
";
        let (result, _, fuel) = run(source, Rights::EXIT);
        assert_eq!(result, Err(Trap::Exited(15)));
        assert!(fuel > 0);
    }

    #[test]
    fn branches_and_booleans() {
        let source = "\
let a = 1
let b = 0
if a and not b or 0 {
    exit 7
} else {
    exit 9
}
";
        let (result, _, _) = run(source, Rights::EXIT);
        assert_eq!(result, Err(Trap::Exited(7)));
    }

    #[test]
    fn reads_the_clock() {
        let source = "exit now_ms()\n";
        let (result, _, _) = run(source, Rights::CLOCK.union(Rights::EXIT));
        assert_eq!(result, Err(Trap::Exited(7)));
    }

    #[test]
    fn arithmetic_precedence_and_division() {
        let source = "exit 2 + 3 * 4 - 10 / 5 % 3\n";
        let (result, _, _) = run(source, Rights::EXIT);
        assert_eq!(result, Err(Trap::Exited(12)));
    }

    #[test]
    fn a_program_without_host_access_needs_no_rights() {
        let source = "\
let x = 2
let y = 3
if x < y {
    x = x + y
}
while x < 10 {
    x = x + 1
}
";
        let (result, lines, _) = run(source, Rights::NONE);
        assert_eq!(result, Ok(None));
        assert!(lines.is_empty());
    }

    #[test]
    fn strings_escape_and_stay_in_one_page() {
        let source = "log \"a\\tb\\n\\\"quoted\\\"\"\n";
        let (_, lines, _) = run(source, Rights::LOG);
        assert_eq!(lines, vec![String::from("a\tb\n\"quoted\"")]);
    }

    #[test]
    fn errors_point_at_the_source() {
        let error = compile("let x = 1\nlet x = 2\n").unwrap_err();
        assert_eq!(error.line, 2);
        assert!(error.message.contains("twice"));
        let error = compile("if 1 { log \"x\" \n").unwrap_err();
        assert!(error.message.contains("'}'"));
        let error = compile("exit nope\n").unwrap_err();
        assert!(error.message.contains("'nope'"));
    }
}
