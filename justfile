default:
    @just --list

fmt:
    cargo fmt --all
    alejandra -q .

fmt-check:
    cargo fmt --all -- --check
    alejandra -q -c .

clippy:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

build:
    cargo build --release

check: fmt-check clippy test

# Evaluate the flake checks, including the NixOS module.
flake-check:
    nix flake check

run *args:
    cargo run -- {{args}}
