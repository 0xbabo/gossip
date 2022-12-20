#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

#[macro_use]
extern crate lazy_static;

mod comms;
mod db;
mod error;
mod globals;
mod settings;
mod ui;

fn main() {
    tracing_subscriber::fmt::init();

    // TBD: start async code

    ui::run();

    // TBD: Tell the async parties to close down
    // TBD: wait for the async parties to close down
}
