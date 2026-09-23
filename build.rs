#[path = "build_support/contracts.rs"]
mod contracts;

fn main() {
    contracts::generate();
}
