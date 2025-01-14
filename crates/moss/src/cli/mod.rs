// SPDX-FileCopyrightText: Copyright © 2020-2023 Serpent OS Developers
//
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;

use clap::{Arg, ArgAction, Command};
use thiserror::Error;

mod extract;
mod index;
mod info;
mod inspect;
mod install;
mod list;
mod remove;
mod repo;
mod state;
mod sync;
mod version;

/// Generate the CLI command structure
fn command() -> Command {
    Command::new("moss")
        .about("Next generation package manager")
        .arg(
            Arg::new("version")
                .short('v')
                .long("version")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("root")
                .short('D')
                .long("directory")
                .global(true)
                .help("Root directory")
                .action(ArgAction::Set)
                .default_value("/")
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("yes")
                .short('y')
                .long("yes-all")
                .global(true)
                .help("Assume yes for all questions")
                .action(ArgAction::SetTrue),
        )
        .arg_required_else_help(true)
        .subcommand(extract::command())
        .subcommand(index::command())
        .subcommand(info::command())
        .subcommand(inspect::command())
        .subcommand(install::command())
        .subcommand(list::command())
        .subcommand(remove::command())
        .subcommand(repo::command())
        .subcommand(state::command())
        .subcommand(sync::command())
        .subcommand(version::command())
}

/// Process all CLI arguments
pub async fn process() -> Result<(), Error> {
    let matches = command().get_matches();
    if matches.get_flag("version") {
        version::print();
        return Ok(());
    }

    let root = matches.get_one::<PathBuf>("root").unwrap();

    match command().get_matches().subcommand() {
        Some(("extract", args)) => extract::handle(args).await.map_err(Error::Extract),
        Some(("index", args)) => index::handle(args).await.map_err(Error::Index),
        Some(("info", args)) => info::handle(args).await.map_err(Error::Info),
        Some(("inspect", args)) => inspect::handle(args).await.map_err(Error::Inspect),
        Some(("install", args)) => install::handle(args, root).await.map_err(Error::Install),
        Some(("list", args)) => list::handle(args).await.map_err(Error::List),
        Some(("remove", args)) => remove::handle(args, root).await.map_err(Error::Remove),
        Some(("repo", args)) => repo::handle(args, root).await.map_err(Error::Repo),
        Some(("state", args)) => state::handle(args, root).await.map_err(Error::State),
        Some(("sync", args)) => sync::handle(args, root).await.map_err(Error::Sync),
        Some(("version", _)) => {
            version::print();
            Ok(())
        }
        None => {
            command().print_help().unwrap();
            Ok(())
        }
        _ => unreachable!(),
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("index")]
    Index(#[from] index::Error),

    #[error("info")]
    Info(#[from] info::Error),

    #[error("install")]
    Install(#[from] install::Error),

    #[error("list")]
    List(#[from] list::Error),

    #[error("inspect")]
    Inspect(#[from] inspect::Error),

    #[error("extract")]
    Extract(#[from] extract::Error),

    #[error("remove")]
    Remove(#[from] remove::Error),

    #[error("repo")]
    Repo(#[from] repo::Error),

    #[error("state")]
    State(#[from] state::Error),

    #[error("sync")]
    Sync(#[from] sync::Error),
}
