#[macro_use]
extern crate lazy_static;

mod comms;
mod error;
mod globals;

fn main() {
    tracing_subscriber::fmt::init();
}
