default:
    @just --list

# The check that should pass on every commit
test:
    cargo test

# The thing that I run periodically
dev:
    cargo fmt --all
    cargo check
    cargo clippy --all-targets --all-features

# Build a python release wheel
wheel *ARGS:
    maturin build --release {{ ARGS }}

# Build a python wheel and run it exactly as `uvx` users would
wheel-run *ARGS: wheel
    uvx --from target/wheels/tzctl-*.whl tzctl {{ ARGS }}
