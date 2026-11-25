# EasyLang Core — Language and Compiler Reference

Version v2.73.0 (`boot/aios-easylang`, milestone M3.3).

EasyLang Core is the source language for AIOS WebAssembly blocks. It is a
small imperative language over `i32` with exactly four host calls, and it is
compiled by `aios-easylang` into the subset that `aios-wasmhost` validates and
runs. There is no external toolchain: the compiler is `#[no_std]` + `alloc`, so
the same code can compile a block inside the kernel later.

## Pipeline

```text
boot/assets/aios/easylang/*.easy
        │  aios-easylang::compile
        ▼
aios/blocks/<stem>.wasm          (one page of strings, one export `run`)
        │  aios-image → FAT32 ESP
        ▼
kernel boot step 6: aios-wasmhost (parse → capability check → run)
```

* One `*.easy` file produces one block. The file stem is the block name:
  `hello.easy` → `blocks/hello.wasm` → policy key `blocks.hello.*`.
* Generated `.wasm` files are build output, not sources: the only committed
  block source is `boot/assets/aios/easylang/hello.easy`.

## Quick start

```powershell
# compile one file next to the source (hello.easy -> hello.wasm)
cargo run --manifest-path boot/aios-easylang/Cargo.toml --release -- hello.easy

# compile into the block directory with an explicit name
cargo run --manifest-path boot/aios-easylang/Cargo.toml --release -- `
    boot/assets/aios/easylang/hello.easy -o boot/out/hello.wasm

# syntax/type check only, no file written
cargo run --manifest-path boot/aios-easylang/Cargo.toml --release -- hello.easy --check
```

The stock image compiles every `.easy` automatically:

```powershell
cargo run --manifest-path boot/aios-image/Cargo.toml --release -- --headless --timeout 20
```

Expected serial proof:

```text
aios-image: compiled hello.easy -> blocks/hello.wasm (95 bytes)
[serial] [block] blocks/hello.wasm: hello from aios-wasmhost
[serial] [block] blocks/hello.wasm ran: entry 'run' (1 page(s))
[serial] [block] blocks/hello.wasm used 5 of 20000 instruction(s)
```

## Program model

* The whole file is the body of the exported function `run`.
* `run` takes no parameters and returns no value. The block runner calls `run`
  (or `_start` for hand-written modules) with an empty argument list.
* There are no user-defined functions, no recursion, no modules and no
  parameters.
* Statements execute top to bottom. `exit` terminates the block; reaching the
  end of the file is a normal return.

## Lexical structure

| Element | Rule |
| --- | --- |
| Encoding | ASCII outside string literals; a non-ASCII byte outside a string is an error. String literals may contain any valid UTF-8. |
| Comments | `#` to the end of the line. |
| Separators | A newline or `;` ends a statement. A newline inside `{}` also separates statements. |
| Identifiers | `[A-Za-z_][A-Za-z0-9_]*`. |
| Keywords | `let`, `if`, `else`, `while`, `log`, `exit`, `yield`, `and`, `or`, `not`, `true`, `false` — always reserved. |
| Integers | Decimal digits only, no sign, no `_`, no `0x`, at most `2147483647` (`i32::MAX`). A negative value is the prefix operator `-`. |
| Strings | Double quotes, single line. Escapes: `\n`, `\t`, `\r`, `\\`, `\"`, `\0`. Other escapes and raw newlines are errors. |
| Positions | 1-based line, 1-based byte column. |

Keywords and identifiers are case-sensitive. `True` is an identifier, not a
boolean.

## Types

EasyLang has one type: signed 32-bit `i32`.

* `true` is `1`, `false` is `0`.
* A condition is false when it is `0` and true otherwise.
* Comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`) and `and`, `or`, `not`
  produce exactly `0` or `1`.
* `+`, `-`, `*` wrap on overflow (WebAssembly semantics).
* `/` and `%` are signed; division or remainder by zero traps with
  `DivideByZero`, and `i32::MIN / -1` traps with `IntegerOverflow`.
* `and` and `or` **do not short-circuit**: both operands are evaluated and then
  normalized to `0`/`1` before the bitwise operation.
* Unary `-x` is emitted as `0 - x`; `not x` is emitted as `x == 0`.

## Statements

| Syntax | Meaning |
| --- | --- |
| `let name = expr` | Declare a variable and initialize it. |
| `name = expr` | Assign to a variable declared earlier in the source. |
| `log "text"` | Write one UTF-8 string to the host log. The argument must be a string literal. |
| `exit expr` | Stop the block with an `i32` status. The kernel treats this as a normal stop. |
| `yield` | Call the host yield hook. |
| `if cond { ... } else { ... }` | Branch. Braces are required; `else` is optional. |
| `while cond { ... }` | Pre-test loop. Braces are required. |

Examples:

```text
# statements separated by newlines
let sum = 0
let i = 5
while i > 0 {
    sum = sum + i
    i = i - 1
}
exit sum
```

```text
# the same program with ';' separators
let x = 2; if x < 3 { log "small" } else { log "large" }
```

An `else` may start on the line after the closing `}` of the `then` branch.
Everything else must be on one line or continue after `{`/`}`/`;`.

## Expressions

Precedence, from loosest to tightest:

| Level | Operators | Associativity |
| --- | --- | --- |
| 1 | `or` | left |
| 2 | `and` | left |
| 3 | `==` `!=` `<` `<=` `>` `>=` | left |
| 4 | `+` `-` | left |
| 5 | `*` `/` `%` | left |
| 6 | prefix `-` `not` | right |
| 7 | literal, variable, `now_ms()`, `true`, `false`, `( expr )` | — |

Because comparisons are left-associative, `a < b < c` parses as
`(a < b) < c`; write `a < b and b < c` instead.

`2 + 3 * 4 - 10 / 5 % 3` evaluates to `12`.

## Variables

* Variables are function-scoped. There are no block scopes.
* A program may declare at most **16** variables (`MAX_LOCALS`).
* A name may be declared once; a second `let` is an error.
* A variable must be declared before it is used **in source order**. Analysis is
  a single pass, so a `let` inside an `if` branch is visible in a later `else`
  branch and after the `if`, even though it does not run on every path.
* WebAssembly locals start at `0`. Reading a variable whose `let` did not
  execute on that path therefore yields `0`.
* `now_ms` is special in expression position: `now_ms` not followed by `()`
  is a syntax error, so do not use it as a variable name.

```text
let total = 0
if now_ms() % 2 == 0 {
    let parity = 1
} else {
    parity = 0          # visible: the `let` was seen earlier in the source
}
total = total + parity  # 0 on the branch where `let` did not run
```

## Strings and memory

* `log` accepts a string literal only. There is no concatenation, formatting,
  interpolation or string variable.
* All literals are concatenated in source order into one blob, placed at offset
  `0` of one data segment and one linear memory page. Identical strings are not
  deduplicated.
* The blob must fit in one page: **65 536 bytes total** across the program,
  otherwise compilation fails.
* A program with no `log` declares no memory at all.
* String bytes are copied verbatim, so UTF-8 and `\0` are preserved. The host
  validates UTF-8 when logging; invalid bytes are reported as
  `<block log is not UTF-8>`.

## Host ABI

Every construct maps to exactly one import. Only the imports a program uses are
emitted, always in this order:

| Construct | Import | Signature | Required right | Runtime effect |
| --- | --- | --- | --- | --- |
| `log "text"` | `aios.log` | `(i32 ptr, i32 len) -> ()` | `log` | `Env::log(text)`; costs `1 + len/64` fuel |
| `exit code` | `aios.exit` | `(i32 code) -> ()` | `exit` | `Trap::Exited(code)`; normal stop for the kernel |
| `now_ms()` | `aios.now_ms` | `() -> i32` | `clock` | `Env::now_ms()` in milliseconds |
| `yield` | `aios.yield` | `() -> ()` | `yield` | `Env::idle()` |

A program that uses no host call needs no rights (`Rights::NONE`). A block
whose `required_rights()` are not covered by its token is skipped before it
executes.

In the kernel, `Env` is `SerialEnv`: `aios.log` becomes
`[serial] [block] <name>: <text>`, `aios.now_ms` reads the PIT tick counter and
`aios.yield` currently calls the default no-op `idle` hook.

## Generated module

For a program with at least one `log`, the compiler writes:

| Section | Content |
| --- | --- |
| Type | `run` signature `() -> ()` plus one signature per used import |
| Import | only the used `aios.*` functions, in `log`, `exit`, `now_ms`, `yield` order |
| Function | exactly one function, `run` |
| Memory | one page, no maximum |
| Export | `run` at the function index after the imports |
| Code | `run` body, at most 16 extra `i32` locals |
| Data | one active segment at offset `0` with the string blob |

Without any `log` there is no memory and no data section. The module never uses
tables, globals, an element section, a data-count section or a start function.

## Command-line interface

```text
aios-easylang <input.easy> [-o <output.wasm>] [--check]
aios-easylang -h | --help
```

| Flag | Behaviour |
| --- | --- |
| `-o`, `--output` | Output path. Default: the input path with a `.wasm` extension. |
| `--check` | Compile and print `<input>: ok (N bytes)`, write nothing. |
| `-h`, `--help` | Print usage and exit successfully. |

* Exactly one input file is accepted.
* The input must be valid UTF-8 (`fs::read_to_string`).
* Exit code `0` on success and `1` on any error.
* Errors go to stderr as `<path>: <line>:<column>: <message>`.

## Image integration

`aios-image` runs with no arguments from the repository root and performs:

1. build `aios-boot` and the kernel;
2. copy `boot/assets` into `boot/out/assets`;
3. compile every `aios/easylang/*.easy` (sorted by file name) into
   `aios/blocks/<stem>.wasm`;
4. delete `aios/easylang` from the staging tree, so the ESP contains the
   compiled block, not the source;
5. write the GPT disk with the FAT32 ESP and verify
   `/aios/blocks/hello.wasm` among the required paths.

The `boot/out` directory is generated and git-ignored. Add a new block by
committing `boot/assets/aios/easylang/<name>.easy` and a policy for it, or rely
on the "no rights" default and the kernel log warning.

## Block policy

Policy lives in a discovered drop-in such as
`boot/assets/aios/config/blocks.conf`:

```ini
# comma-separated subset of log, exit, clock, yield
blocks.hello.rights = log
# instruction budget; absent means DEFAULT_FUEL (100000)
blocks.hello.fuel = 20000
```

Both keys are `Effective` in `config::KeyPolicy`: the WASM host consumes
`blocks.<stem>.rights` and `blocks.<stem>.fuel` at boot. Other shipped
configuration keys (`system.*`, `boot.*`, `net.*`) are Informational and are
only parsed, validated and logged.

* A missing `blocks.<stem>.rights` grants `Rights::NONE` and the kernel logs a
  warning; a block that needs any host access is then skipped.
* An unknown right name skips the block.
* `blocks.<stem>.fuel` must fit in `u32`; a missing or malformed value falls
  back to `DEFAULT_FUEL`.
* Blocks run once at boot, before the scheduler, so fuel is the only time
  limit. A trap or an exhausted budget marks the block failed but never stops
  the kernel.

## Diagnostics

Every error carries a position and a lowercase message without a trailing
period:

```text
hello.easy: 3:5: unknown variable 'nope'
hello.easy: 1:1: variable 'x' is declared twice
hello.easy: 4:9: log expects a string literal, found 'x'
hello.easy: 2:1: expected a new line or ';' after the statement
```

Common errors:

| Message | Cause |
| --- | --- |
| `integer literal is too large` | Value above `2147483647`, or a negative literal written without unary `-`. |
| `a number cannot be followed by a letter` | `1x` instead of `1` and a separate identifier. |
| `unterminated string` / `unknown escape '\x'` | Bad literal; only `\n \t \r \\ \" \0` are supported. |
| `expected a new line or ';' after the statement` | Two statements on one line without `;`. |
| `log expects a string literal` | `log x` or `log 1`. |
| `variable 'x' is declared twice` | Duplicate `let`; shadowing is not allowed. |
| `unknown variable 'x'` | Use before the `let` that declares it, or a typo. |
| `cannot assign to unknown variable 'x'` | Assignment without a prior `let`. |
| `too many variables (at most 16)` | More than 16 names. |
| `the logged text does not fit in one memory page (65536 bytes)` | String blob too large. |
| `blocks are nested too deeply (at most 64)` | More than 64 control frames; each `if` is one, each `while` two. |
| `expected '{'` / `expected '}'` | Missing braces around a branch or loop body. |
| `expected an expression` | Empty right-hand side, e.g. `exit`. |

The column counts bytes on the line. It is exact for the ASCII syntax and
points at the first byte of a multi-byte character inside a string.

## Limits

| Item | Limit | Enforced by |
| --- | --- | --- |
| Variable count | 16 | `aios-easylang` (`MAX_LOCALS`) |
| Control-frame nesting | 64 (`if` = 1, `while` = 2) | `aios-easylang` / `aios-wasmhost` |
| String blob | 65 536 bytes | `aios-easylang` |
| Linear memory | 1 page (host allows 16) | `aios-easylang` |
| Integer literal | `i32::MAX` | lexer |
| Function calls / call depth | none (no user functions) | language |
| Loop iterations | fuel only | runtime |
| Fuel | `blocks.<stem>.fuel`, default `100000` | kernel |
| Execution | once at boot, single-threaded | kernel block runner |

## Examples

### Hello

```text
# boot/assets/aios/easylang/hello.easy
log "hello from aios-wasmhost"
```

Needs `blocks.hello.rights = log`.

### Sum and status

```text
let sum = 0
let i = 5
while i > 0 {
    sum = sum + i
    i = i - 1
}
exit sum          # Trap::Exited(15), a normal stop
```

Needs `exit`; uses no memory beyond the code.

### Clock and cooperative yield

```text
let start = now_ms()
let spins = 0
while now_ms() - start < 100 {
    spins = spins + 1
    yield
}
log "waited 100 ms"
exit spins
```

Needs `clock`, `yield`, `log` and `exit`.

## Non-goals

EasyLang Core deliberately has no user functions, arrays, string values,
floating point, short-circuit logic, imports beyond the four host calls, or an
in-language way to declare capabilities. Blocks stay boot-time only until the
scheduler can host them as processes (M4); at that point the same compiler can
run in the kernel and the same policy keys apply.

## Tests

The crate ships 21 tests: 6 lexer, 5 parser, 2 LEB128 encoders and 8
integration tests that compile a program, parse the bytes with
`aios_wasmhost::Module`, instantiate it with a `Token` and run `run`. They cover
strings and escapes, arithmetic precedence, loops, booleans, the clock, denied
rights, fuel accounting and source positions.

```powershell
cargo test --manifest-path boot/aios-easylang/Cargo.toml
cargo clippy --manifest-path boot/aios-easylang/Cargo.toml -- -D warnings
cargo fmt --manifest-path boot/aios-easylang/Cargo.toml --check
```
