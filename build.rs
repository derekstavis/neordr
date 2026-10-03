#[path = "build_support/contracts.rs"]
mod contracts;

#[path = "build_support/detection.rs"]
mod detection;

fn main() {
    contracts::generate();
    detection::generate();
}
