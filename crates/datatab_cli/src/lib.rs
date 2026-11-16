//! Command-line parsing and application launching.
mod launcher;

use clap::{Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "datatab",
    version,
    about,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[arg(long, hide = true, conflicts_with = "path")]
    gui: bool,
    /// Script files to open, in order.
    #[arg(value_name = "PATH")]
    path: Vec<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the MCP server for an AI client.
    #[command(
        after_help = "The client ID is a shared identifier, not a credential. Any local process that knows it can connect. Do not expose the server beyond localhost without additional authentication."
    )]
    Mcp {
        /// Identifier of a trusted client.
        #[arg(long)]
        client_id: String,
        /// Config directory (accepted for compatibility).
        #[arg(long)]
        config_dir: Option<PathBuf>,
    },
}

/// Parse and validate before touching the socket or application data.
pub fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut cli = Cli::try_parse_from(args)?;
    let invalid: Vec<_> = cli.path.iter().filter(|path| !path.is_file()).collect();
    if !invalid.is_empty() {
        let paths = invalid
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(<Cli as clap::CommandFactory>::command().error(
            clap::error::ErrorKind::ValueValidation,
            format!("scripts are not existing files: {paths}"),
        ));
    }
    cli.path = cli
        .path
        .into_iter()
        .map(|path| {
            std::path::absolute(&path).map_err(|error| {
                <Cli as clap::CommandFactory>::command().error(
                    clap::error::ErrorKind::ValueValidation,
                    format!("cannot resolve script {}: {error}", path.display()),
                )
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(cli)
}

/// Action to perform after parsing.
pub enum Dispatch {
    Exit(i32),
    RunGui,
    Mcp {
        client_id: String,
        config_dir: Option<PathBuf>,
    },
}

impl Cli {
    pub fn run(self) -> Dispatch {
        if let Some(Command::Mcp {
            client_id,
            config_dir,
        }) = self.command
        {
            return Dispatch::Mcp {
                client_id,
                config_dir,
            };
        }
        if self.gui {
            return Dispatch::RunGui;
        }
        if !self.path.is_empty() {
            return Dispatch::Exit(launcher::open(&self.path));
        }
        dispatch_focus(launcher::focus())
    }
}

fn dispatch_focus(result: Result<(), datatab_ipc::AppControlError>) -> Dispatch {
    match result {
        Ok(()) => Dispatch::Exit(0),
        Err(datatab_ipc::AppControlError::Unavailable(_)) => Dispatch::RunGui,
        Err(datatab_ipc::AppControlError::Request(error)) => {
            eprintln!("Failed to focus DataTab: {error}");
            Dispatch::Exit(1)
        }
    }
}

#[cfg(test)]
mod tests;
