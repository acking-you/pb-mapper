//! `pb_mapper_ui <verb> …` — the command-line half of the binary.
//!
//! All of it lives here rather than in Dart for two reasons. Dart cannot talk
//! to a Windows named pipe (`dart:io` covers unix sockets but not named pipes),
//! so a Dart client would have to fall back to a localhost TCP port that any
//! process could reach. And phase 0 measured that Dart's stdout does not even
//! work in this situation: `AttachConsole` populates `GetStdHandle`, which is
//! what Rust writes through, but leaves the C runtime descriptor Dart uses
//! unbound.
//!
//! Dart's whole part is: hand argv over, print nothing, exit with what comes
//! back.

use std::ffi::{c_char, c_int, CStr};

use clap::{CommandFactory, Parser};

use crate::ctl::proto::Response;
use crate::ctl::{endpoint, server, Command};

/// Returned when argv is not a command at all, so Dart knows to run the GUI.
/// Chosen so it cannot collide with a real exit code.
pub const NOT_A_COMMAND: c_int = -1;

const EXIT_OK: c_int = 0;
const EXIT_FAILED: c_int = 1;
const EXIT_USAGE: c_int = 2;
const EXIT_NO_UI: c_int = 3;

#[derive(Parser)]
#[command(
    name = "pb-mapper",
    about = "Control a running pb-mapper UI",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Print the raw JSON envelope instead of a human summary.
    #[arg(long, global = true)]
    json: bool,
}

/// Whether the first non-flag argument names a subcommand.
///
/// The verb set comes from clap, which gets it from the `Command` enum, so
/// there is exactly one definition of what a verb is. Everything Flutter, the
/// OS and the debugger pass begins with `-` (`--observatory-port=…`, macOS's
/// `-psn_0_123456`), so a normal launch never matches.
fn looks_like_command(args: &[String]) -> bool {
    let Some(first) = args.iter().find(|a| !a.starts_with('-')) else {
        return false;
    };
    Cli::command()
        .get_subcommands()
        .any(|sub| sub.get_name() == first)
}

/// Render a successful response for a person.
fn print_human(response: &Response) {
    if let Some(message) = &response.message {
        println!("{message}");
    }
    let Some(data) = &response.data else { return };
    match serde_json::to_string_pretty(data) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{data}"),
    }
}

fn run(args: Vec<String>) -> c_int {
    if !looks_like_command(&args) {
        return NOT_A_COMMAND;
    }

    // clap wants argv[0]; Dart hands us only the arguments.
    let mut argv = vec!["pb-mapper".to_string()];
    argv.extend(args);

    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(e) => {
            // clap already formats help and usage errors well.
            let _ = e.print();
            return if e.use_stderr() { EXIT_USAGE } else { EXIT_OK };
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("could not start a runtime: {e}");
            return EXIT_FAILED;
        }
    };

    let mutating = cli.command.is_mutating();
    runtime.block_on(async move {
        let response = match server::request(cli.command).await {
            Ok(response) => response,
            Err(e) if endpoint::is_not_listening(&e) => {
                // Attached is all there is for now; headless arrives in phase 3.
                // Say which it was rather than a bare connection error.
                eprintln!("No running pb-mapper UI to attach to.");
                if mutating {
                    eprintln!("Start the UI and try again.");
                }
                return EXIT_NO_UI;
            }
            Err(e) => {
                eprintln!("could not reach the pb-mapper UI: {e}");
                return EXIT_FAILED;
            }
        };

        if cli.json {
            match serde_json::to_string(&response) {
                Ok(text) => println!("{text}"),
                Err(e) => eprintln!("could not render the response: {e}"),
            }
        } else if let Some(error) = response.as_error() {
            eprintln!("{error}");
        } else {
            print_human(&response);
        }

        if response.success {
            EXIT_OK
        } else {
            EXIT_FAILED
        }
    })
}

/// Entry point for the Dart side.
///
/// Returns [`NOT_A_COMMAND`] when argv is a normal launch, in which case the
/// caller should carry on and open a window.
///
/// # Safety
/// `argv` must point to `argc` valid, NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_cli_main(argc: c_int, argv: *const *const c_char) -> c_int {
    if argv.is_null() || argc <= 0 {
        return NOT_A_COMMAND;
    }
    let mut args = Vec::with_capacity(argc as usize);
    for index in 0..argc as usize {
        let ptr = unsafe { *argv.add(index) };
        if ptr.is_null() {
            return NOT_A_COMMAND;
        }
        match unsafe { CStr::from_ptr(ptr) }.to_str() {
            Ok(text) => args.push(text.to_string()),
            Err(_) => return NOT_A_COMMAND,
        }
    }
    run(args)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The one thing Dart would otherwise have to know. Getting it wrong in the
    /// permissive direction means a launch tries to run a command; in the
    /// strict direction it means a command opens a window.
    #[test]
    fn a_launch_is_never_mistaken_for_a_command() {
        assert!(!looks_like_command(&args(&[])));
        assert!(!looks_like_command(&args(&["--observatory-port=1234"])));
        assert!(!looks_like_command(&args(&["-psn_0_123456"])));
        assert!(!looks_like_command(&args(&["--verbose"])));
        assert!(!looks_like_command(&args(&["not-a-verb"])));
    }

    #[test]
    fn every_verb_is_recognised() {
        for verb in ["hello", "status", "services", "clients", "register"] {
            assert!(
                looks_like_command(&args(&[verb])),
                "{verb} should be recognised"
            );
        }
        // And with global flags in front, which is where a hand-written check
        // would usually go wrong.
        assert!(looks_like_command(&args(&["--json", "status"])));
    }

    /// The verb list is derived from `Command`, so this fails the moment a
    /// variant is added without the CLI knowing about it.
    #[test]
    fn the_verb_list_comes_from_the_command_enum() {
        let verbs: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|s| s.get_name().to_string())
            .collect();
        assert!(verbs.contains(&"hello".to_string()));
        assert!(verbs.contains(&"connections".to_string()));
        assert!(
            verbs.len() >= 10,
            "expected the whole surface, got {verbs:?}"
        );
    }
}
