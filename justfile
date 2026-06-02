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

# Run the full Windows visual harness (all cases)
test-windows:
    pwsh -File testing/workflows/windows.ps1 -All

# Run a single Windows harness case by id
test-windows-case CASE:
    pwsh -File testing/workflows/windows.ps1 -Case {{CASE}}

# Build the Windows binary then run the full Windows harness
test-windows-build:
    pwsh -File testing/workflows/windows.ps1 -Build -All
