default:
    @just --list

# Run the visual harness on a platform (mac|windows)
visual platform="mac" *args="":
    bash testing/workflows/{{platform}}.sh {{args}}

# Run a single case by id on a platform
visual-case id platform="mac":
    CASE_ID={{id}} bash testing/workflows/{{platform}}.sh

# Release build of the mac binary (used by the visual harness)
build:
    cargo build --release -p sonicterm-mac
