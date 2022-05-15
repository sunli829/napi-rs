use crate::utils::{Executable, SimpleLogger};
use clap::{Parser, Subcommand};
use std::convert::TryFrom;

mod build;
mod new;

use build::*;
use new::*;

#[derive(Parser)]
#[clap(name = "napi", bin_name = "napi", version, about, long_about = None)]
struct Cli {
  #[clap(subcommand)]
  command: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
  New(Box<NewCommandArgs>),
  Build(Box<BuildCommandArgs>),
}

macro_rules! run_command {
  ( $src:expr, $( ($branch:ident, $cmd:ty) ),* ) => {
    match $src {
      $(
        SubCommand::$branch(args) => {
          <$cmd>::try_from(*args)
            .and_then(|mut cmd| cmd.execute())
            .unwrap_or_else(|_| {
              std::process::exit(1);
            });
        }
      ),*
      #[allow(unreachable_patterns)]
      _ => unreachable!(),
    }
  };
}

pub fn run(args: Vec<String>) {
  let cli = Cli::parse_from(args);

  // eat the error of setting logger
  if log::set_boxed_logger(Box::new(SimpleLogger)).is_err() {}
  log::set_max_level(log::LevelFilter::Trace);

  run_command!(
    cli.command,
    (New, new::NewCommand),
    (Build, build::BuildCommand)
  );
}
