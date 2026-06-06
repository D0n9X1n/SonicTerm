default:
    @just --list

# Release build of the mac binary.
build:
    cargo build --release -p sonicterm-mac

# Debug build of the app crate.
build-app:
    cargo build -p sonicterm-app

# Debug build of the mac binary.
build-mac:
    cargo build -p sonicterm-mac
