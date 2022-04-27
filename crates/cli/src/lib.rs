#[macro_use]
extern crate napi_derive;

mod commands;
mod utils;

#[napi]
pub fn run(args: Vec<String>) {
  commands::run(args);
}
