# Contributing to open-cosmo

Thank you for your interest in contributing to open-cosmo! This document provides guidelines and instructions for contributing.

For day-to-day build/test/run commands, repository layout, and
troubleshooting, see [`DEVELOPMENT.md`](DEVELOPMENT.md). This document
covers process: how to propose, branch, commit, and submit changes.

## Code of Conduct

All contributors are expected to follow our Code of Conduct. Be respectful, inclusive, and constructive in all interactions.

## Getting Started

### Prerequisites
- Rust 1.75+ ([Install Rust](https://rustup.rs/); `rustup component add rustfmt clippy`)
- PostgreSQL 14+ (only needed for the `open-runo-db` `postgres` feature — see `DEVELOPMENT.md` §5)
- Git

### Local Setup

```bash
# Clone the repository
git clone https://github.com/aon-co-jp/open-cosmo.git
cd open-cosmo

# Build the project
cargo build

# Run tests
cargo test

# Check code quality
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings

# Run the quality gate pipeline
make quality-gate
```

## Development Workflow

### Branch Naming
- `feature/` for new features
- `fix/` for bug fixes
- `docs/` for documentation
- `refactor/` for refactoring
- `test/` for test improvements

### Commit Messages
Follow conventional commits:
- `feat: add AI routing engine`
- `fix: resolve federation schema composition issue`
- `docs: update installation guide`
- `test: add integration tests for backup system`

### Pull Request Process

1. Create a feature branch from `main`
2. Make your changes with clear commits
3. Add/update tests for new functionality
4. Run quality gates: `make quality-gate`
5. Update documentation if needed
6. Submit PR with description of changes

### Code Style

open-cosmo follows Rust conventions:

```bash
# Auto-format code
cargo fmt

# Check linting
cargo clippy --all-targets --all-features

# Run security audit
cargo audit
```

## Testing

- Write unit tests alongside code (`#[cfg(test)]`)
- Add integration tests in `tests/` directories
- Maintain test coverage above 70%
- Test documentation examples: `cargo test --doc`

Example:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_initialization() {
        // Test implementation
    }
}
```

## Documentation

- Add doc comments to public APIs: `/// Description`
- Update README for major features
- Add architecture diagrams for complex systems
- Keep CHANGELOG.md updated

## Reporting Issues

When reporting bugs:
- Include Rust version (`rustc --version`)
- Include open-cosmo version
- Provide minimal reproducible example
- Describe expected vs actual behavior

## Feature Proposals

For major features:
1. Open a GitHub Discussion first
2. Describe the use case and design
3. Get feedback from maintainers
4. Create implementation plan
5. Submit PR when approved

## License

By contributing, you agree that your contributions will be licensed under the same license as the project (Apache License 2.0 / MIT).

## Questions?

- Open an issue for bug reports
- Start a Discussion for questions
- Check existing documentation

Thank you for contributing to open-cosmo! 🚀
