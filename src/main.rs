//! reims-util: makes the OpenCore boot image and OVMF an x86-64 macOS VM boots through, and the
//! macOS installers it installs from, out of Apple's own files: online (Apple's recovery image)
//! and offline (a disk image of one exact release). With no command it opens its interactive
//! interface (tui.rs), in the terminal's lines rather than a full screen; each job is also a
//! command (cli.rs).

mod cache;
mod chunklist;
mod cli;
mod http;
mod image;
mod installer;
mod opencore;
mod ovmf;
mod progress;
mod recovery;
mod tui;

use clap::{CommandFactory, Parser};
use std::io::IsTerminal;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = cli::Cli::parse();
    let result = match args.command {
        Some(c) => cli::run(c, args.json),
        None if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && !args.json => {
            tui::run()
        }
        None => {
            let _ = cli::Cli::command().print_help();
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is::<progress::Cancelled>() => {
            eprintln!("Stopped. Run the same command again to resume.");
            ExitCode::from(130)
        }
        Err(e) => {
            if !args.json {
                eprintln!("error: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}
