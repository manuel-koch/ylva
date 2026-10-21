# Git hooks and conventional commits

We should establish git hooks to

- prevent commits on main
- conventional commit message
- conventional branch naming

Which tool to choose to establish the required checks and guidelines ?
Which tool has better reputation ?
Which tool fits better for our Rust ecosystem ?

- `pre-commit`
- `lefthook`
- alternatives ?

Should test, linting or formatting be part of commit hooks
or just control CI pipeline verdict ?

## CI/CD

How to setup CI/CD stuff in Github ?

Builds for feature branches ?

Automatic release on main commits ?
How to control the cargo version ( semantic versioning, tooling ) ?
